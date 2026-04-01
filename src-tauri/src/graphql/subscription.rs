//! GraphQL subscription resolvers for qontinui-runner.
//!
//! Subscriptions provide typed, real-time event streams over WebSocket,
//! replacing the untyped broadcast channel + SSE/WS pattern.
//!
//! Available subscriptions:
//! - `uiBridgeHealthStream` — periodic health snapshots (replaces polling GET /health)
//! - `runnerEvents` — all runner events with optional type filter
//! - `taskRunProgress` — events for a specific task run
//! - `findingUpdates` — finding changes for a specific task run (polled from DB)
//! - `orchestrationLoopStatus` — periodic orchestration loop state

use async_graphql::*;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;

use crate::mcp::types::ApiState;

use super::query::build_health_snapshot;
use super::types::*;

pub struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    /// Health status stream — pushes UI Bridge health at a configurable interval.
    /// Replaces polling GET /health with server push.
    async fn ui_bridge_health_stream(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 3000)] interval_ms: i32,
    ) -> Result<impl futures_util::Stream<Item = UiBridgeHealth>> {
        let state = ctx.data::<Arc<ApiState>>()?.clone();
        let interval = std::time::Duration::from_millis(interval_ms.max(500) as u64);

        Ok(async_stream::stream! {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                yield build_health_snapshot(&state).await;
            }
        })
    }

    /// Typed runner event stream — replaces untyped /ws/events endpoint.
    /// Optional filter to receive only specific event types.
    async fn runner_events(
        &self,
        ctx: &Context<'_>,
        filter: Option<Vec<String>>,
    ) -> Result<impl futures_util::Stream<Item = RunnerEvent>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let rx = state.app_state.event_broadcast.subscribe();

        Ok(BroadcastStream::new(rx).filter_map(move |result| {
            let filter = filter.clone();
            async move {
                let raw = result.ok()?;
                let event = convert_raw_event_to_runner_event(&raw)?;

                // Apply optional filter
                if let Some(ref types) = filter {
                    let event_type = runner_event_type_name(&event);
                    if !types.iter().any(|t| t == event_type) {
                        return None;
                    }
                }

                Some(event)
            }
        }))
    }

    /// Task run progress stream — replaces 3s polling for task run status.
    /// Filters events to only those matching the specified task run ID.
    async fn task_run_progress(
        &self,
        ctx: &Context<'_>,
        task_run_id: String,
    ) -> Result<impl futures_util::Stream<Item = RunnerEvent>> {
        let state = ctx.data::<Arc<ApiState>>()?;
        let rx = state.app_state.event_broadcast.subscribe();

        Ok(BroadcastStream::new(rx).filter_map(move |result| {
            let task_run_id = task_run_id.clone();
            async move {
                let raw = result.ok()?;
                let event = convert_raw_event_to_runner_event(&raw)?;

                // Filter to only events for this task run
                match &event {
                    RunnerEvent::StepProgress(e) if e.task_run_id == task_run_id => Some(event),
                    RunnerEvent::TaskRunUpdate(e) if e.task_run_id == task_run_id => Some(event),
                    RunnerEvent::OrchestratorStateChange(e) if e.task_run_id == task_run_id => {
                        Some(event)
                    }
                    RunnerEvent::AiOutputChunk(e) if e.task_run_id == task_run_id => Some(event),
                    _ => None,
                }
            }
        }))
    }

    /// Finding updates stream — polls DB for finding changes on a task run.
    /// Emits the full findings list whenever the count or statuses change.
    /// Auto-closes when the task reaches a terminal status or after 30 minutes.
    async fn finding_updates(
        &self,
        ctx: &Context<'_>,
        task_run_id: String,
        #[graphql(default = 2000)] interval_ms: i32,
    ) -> Result<impl futures_util::Stream<Item = Vec<GqlFinding>>> {
        let state = ctx.data::<Arc<ApiState>>()?.clone();
        let interval = std::time::Duration::from_millis(interval_ms.max(500) as u64);

        Ok(async_stream::stream! {
            let mut ticker = tokio::time::interval(interval);
            let mut last_hash: u64 = 0;
            let started = std::time::Instant::now();
            let max_duration = std::time::Duration::from_secs(30 * 60); // 30 min TTL
            let mut terminal_ticks = 0u32; // count ticks after task finishes

            loop {
                ticker.tick().await;

                // TTL: auto-close after max duration
                if started.elapsed() > max_duration {
                    break;
                }

                let trid = task_run_id.clone();

                // Check task status — stop polling if terminal
                let task_status = state.app_state.pg_db.get_task_run(&trid).await.ok().flatten().map(|t| t.status);

                let is_terminal = matches!(
                    task_status.as_deref(),
                    Some("complete") | Some("failed") | Some("stopped")
                );
                let findings = match state.app_state.pg_db.get_findings_for_task(&trid).await {
                    Ok(f) => f,
                    _ => continue,
                };

                // Compute a simple change hash (count + status string)
                let hash = {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = DefaultHasher::new();
                    findings.len().hash(&mut hasher);
                    for f in &findings {
                        f.id.hash(&mut hasher);
                        f.status.as_str().hash(&mut hasher);
                        f.updated_at.hash(&mut hasher);
                    }
                    hasher.finish()
                };

                if hash != last_hash {
                    last_hash = hash;
                    yield findings.into_iter().map(GqlFinding::from_domain).collect();
                }

                // After task finishes, emit one more update then close
                if is_terminal {
                    terminal_ticks += 1;
                    if terminal_ticks >= 2 {
                        break;
                    }
                }
            }
        })
    }

    /// Orchestration loop status stream — pushes loop state at a configurable interval.
    /// Auto-closes after the loop reaches a terminal phase (Complete/Stopped/Error)
    /// or after 2 hours.
    async fn orchestration_loop_status_stream(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 2000)] interval_ms: i32,
    ) -> Result<impl futures_util::Stream<Item = GqlOrchestrationLoopStatus>> {
        let state = ctx.data::<Arc<ApiState>>()?.clone();
        let interval = std::time::Duration::from_millis(interval_ms.max(500) as u64);

        Ok(async_stream::stream! {
            let mut ticker = tokio::time::interval(interval);
            let started = std::time::Instant::now();
            let max_duration = std::time::Duration::from_secs(2 * 60 * 60); // 2h TTL
            let mut terminal_ticks = 0u32;

            loop {
                ticker.tick().await;

                if started.elapsed() > max_duration {
                    break;
                }

                let status = crate::orchestration_loop::loop_engine::get_status_compat(
                    state.app_state.orchestration_loops.clone(),
                )
                .await;

                let phase_str = format!("{:?}", status.phase);
                let is_terminal = matches!(
                    phase_str.as_str(),
                    "Complete" | "Stopped" | "Error"
                );

                yield GqlOrchestrationLoopStatus {
                    running: status.running,
                    phase: phase_str,
                    current_iteration: status.current_iteration as i32,
                    started_at: status.started_at,
                    error: status.error,
                };

                // After loop finishes, emit final state then close
                if is_terminal && !status.running {
                    terminal_ticks += 1;
                    if terminal_ticks >= 2 {
                        break;
                    }
                }
            }
        })
    }

    /// Multi-loop status stream — pushes aggregated status of all loops at a configurable interval.
    /// Auto-closes after all loops complete/stop/error or after 2 hours.
    async fn multi_orchestration_loop_status_stream(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 2000)] interval_ms: i32,
    ) -> Result<impl futures_util::Stream<Item = GqlMultiLoopStatus>> {
        let state = ctx.data::<Arc<ApiState>>()?.clone();
        let interval = std::time::Duration::from_millis(interval_ms.max(500) as u64);

        Ok(async_stream::stream! {
            let mut ticker = tokio::time::interval(interval);
            let started = std::time::Instant::now();
            let max_duration = std::time::Duration::from_secs(2 * 60 * 60);
            let mut terminal_ticks = 0u32;

            loop {
                ticker.tick().await;

                if started.elapsed() > max_duration {
                    break;
                }

                let multi_status = crate::orchestration_loop::loop_engine::get_multi_status(
                    state.app_state.orchestration_loops.clone(),
                )
                .await;

                let all_complete = multi_status.all_complete;
                let any_error = multi_status.any_error;
                let has_loops = !multi_status.loops.is_empty();

                let loops: Vec<GqlLoopInstanceStatus> = multi_status
                    .loops
                    .into_iter()
                    .map(|ls| GqlLoopInstanceStatus {
                        loop_id: ls.loop_id,
                        label: ls.label,
                        running: ls.status.running,
                        phase: format!("{:?}", ls.status.phase),
                        current_iteration: ls.status.current_iteration as i32,
                        max_iterations: ls.status.max_iterations as i32,
                        workflow_id: ls.status.workflow_id,
                        target_runner_port: ls.status.target_runner_port as i32,
                        target_runner_id: ls.status.target_runner_id,
                        is_pipeline: ls.status.is_pipeline,
                        started_at: ls.status.started_at,
                        error: ls.status.error,
                    })
                    .collect();

                yield GqlMultiLoopStatus {
                    loops,
                    all_complete,
                    any_error,
                    stop_all_on_error: multi_status.stop_all_on_error,
                };

                // Auto-close when all loops are done
                if has_loops && (all_complete || any_error) {
                    terminal_ticks += 1;
                    if terminal_ticks >= 2 {
                        break;
                    }
                }
            }
        })
    }
}

/// Convert a raw broadcast event (serde_json::Value) to a typed RunnerEvent.
fn convert_raw_event_to_runner_event(raw: &serde_json::Value) -> Option<RunnerEvent> {
    // The broadcast channel sends events serialized from AppEvent.
    // The EventBroadcaster wraps them as {"channel": name, "payload": event}
    let payload = raw.get("payload").unwrap_or(raw);
    let event_type = payload
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match event_type {
        "OrchestratorStateChange" => {
            let data = payload.get("data")?;
            Some(RunnerEvent::OrchestratorStateChange(
                OrchestratorStateChangeEvent {
                    task_run_id: data.get("task_run_id")?.as_str()?.to_string(),
                    workflow_stage: data
                        .get("workflow_stage")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    iteration: data.get("iteration").and_then(|v| v.as_i64()).map(|v| v as i32),
                    phase: data
                        .get("phase")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    state_data: data.get("state_data").map(|v| Json(v.clone())),
                },
            ))
        }
        "StepProgress" => {
            let data = payload.get("data")?;
            Some(RunnerEvent::StepProgress(StepProgressEvent {
                task_run_id: data.get("task_run_id")?.as_str()?.to_string(),
                step_index: data.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                step_name: data
                    .get("step_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                status: data
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                details: data.get("details").map(|v| Json(v.clone())),
                timestamp: data
                    .get("timestamp")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .to_string(),
            }))
        }
        "TaskRunUpdate" => {
            let data = payload.get("data")?;
            Some(RunnerEvent::TaskRunUpdate(TaskRunUpdateEvent {
                task_run_id: data.get("task_run_id")?.as_str()?.to_string(),
                status: data
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                iteration: data.get("iteration").and_then(|v| v.as_i64()).map(|v| v as i32),
                details: data.get("details").map(|v| Json(v.clone())),
                timestamp: data
                    .get("timestamp")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .to_string(),
            }))
        }
        "FindingDetected" => {
            let data = payload.get("data")?;
            Some(RunnerEvent::FindingDetected(FindingDetectedEvent {
                finding: Json(data.get("finding").cloned().unwrap_or(data.clone())),
            }))
        }
        "FindingResolved" => {
            let data = payload.get("data")?;
            Some(RunnerEvent::FindingResolved(FindingResolvedEvent {
                finding: Json(data.get("finding").cloned().unwrap_or(data.clone())),
            }))
        }
        "AiOutputChunk" => {
            let data = payload.get("data")?;
            Some(RunnerEvent::AiOutputChunk(AiOutputChunkEvent {
                task_run_id: data.get("task_run_id")?.as_str()?.to_string(),
                chunk: data
                    .get("chunk")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                accumulated_length: data
                    .get("accumulated_length")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32,
            }))
        }
        _ => {
            // Wrap unknown event types as GenericRunnerEvent
            Some(RunnerEvent::GenericEvent(GenericRunnerEvent {
                event_type: event_type.to_string(),
                data: Json(payload.clone()),
            }))
        }
    }
}

/// Get the discriminant name for a RunnerEvent (for filtering).
fn runner_event_type_name(event: &RunnerEvent) -> &'static str {
    match event {
        RunnerEvent::OrchestratorStateChange(_) => "OrchestratorStateChange",
        RunnerEvent::StepProgress(_) => "StepProgress",
        RunnerEvent::TaskRunUpdate(_) => "TaskRunUpdate",
        RunnerEvent::FindingDetected(_) => "FindingDetected",
        RunnerEvent::FindingResolved(_) => "FindingResolved",
        RunnerEvent::AiOutputChunk(_) => "AiOutputChunk",
        RunnerEvent::GenericEvent(_) => "GenericEvent",
    }
}
