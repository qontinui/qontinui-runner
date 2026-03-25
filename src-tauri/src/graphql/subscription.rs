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

            loop {
                ticker.tick().await;
                let db = state.app_state.checkpoint_db.clone();
                let trid = task_run_id.clone();

                let findings = match tokio::task::spawn_blocking(move || {
                    db.get_findings_for_task(&trid)
                }).await {
                    Ok(Ok(f)) => f,
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
            }
        })
    }

    /// Orchestration loop status stream — pushes loop state at a configurable interval.
    async fn orchestration_loop_status_stream(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 2000)] interval_ms: i32,
    ) -> Result<impl futures_util::Stream<Item = GqlOrchestrationLoopStatus>> {
        let state = ctx.data::<Arc<ApiState>>()?.clone();
        let interval = std::time::Duration::from_millis(interval_ms.max(500) as u64);

        Ok(async_stream::stream! {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let loop_state = state
                    .app_state
                    .orchestration_loop
                    .lock()
                    .await;

                yield GqlOrchestrationLoopStatus {
                    running: loop_state.running,
                    phase: format!("{:?}", loop_state.phase),
                    current_iteration: loop_state.current_iteration as i32,
                    started_at: loop_state
                        .started_at
                        .map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
                    error: loop_state.error.clone(),
                };
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
                    iteration: data.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
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
