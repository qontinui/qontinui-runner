//! Cascade detection SSE endpoints
//!
//! Provides endpoints for monitoring cascade detection events from the qontinui
//! Python library's image recognition pipeline. Cascade events indicate which
//! backend (OpenCV, PyTorch, etc.) was tried and whether it hit or missed.
//!
//! ## Endpoints
//!
//! - `GET /cascade/events` — Returns recent cascade events as a JSON array (ring buffer, max 200)
//! - `GET /cascade/stream` — SSE stream of new cascade events with 30s keepalive

use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive},
        Json, Sse,
    },
    routing::get,
    Router,
};
use futures_util::StreamExt as FuturesStreamExt;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, warn};

use crate::mcp::types::ApiState;

/// Maximum number of cascade events retained in the ring buffer.
const MAX_RING_BUFFER_SIZE: usize = 200;

/// Global cascade event ring buffer, initialized once.
static CASCADE_BUFFER: OnceLock<Arc<Mutex<VecDeque<Value>>>> = OnceLock::new();

/// Get or initialize the cascade buffer.
fn get_buffer() -> &'static Arc<Mutex<VecDeque<Value>>> {
    CASCADE_BUFFER
        .get_or_init(|| Arc::new(Mutex::new(VecDeque::with_capacity(MAX_RING_BUFFER_SIZE))))
}

/// Check whether a broadcast event is cascade-related.
///
/// Cascade events can arrive in two forms depending on the emission path:
///
/// 1. Python executor path: raw JSON with `"event_type"` or `"event"` containing "cascade"
/// 2. EventBroadcaster path: `{"channel": "...", "payload": {"event_type": "...", "data": {...}}}`
fn is_cascade_event(event: &Value) -> bool {
    // Check direct event_type field (Python executor events via broadcast_to_websocket)
    if let Some(et) = event.get("event_type").and_then(|v| v.as_str()) {
        if et.contains("cascade") {
            return true;
        }
    }

    // Check event field (Python executor "Event" messages)
    if let Some(ev) = event.get("event").and_then(|v| v.as_str()) {
        if ev.contains("cascade") {
            return true;
        }
    }

    // Check nested data.event_type (some events nest the type inside data)
    if let Some(data) = event.get("data") {
        if let Some(et) = data.get("event_type").and_then(|v| v.as_str()) {
            if et.contains("cascade") {
                return true;
            }
        }
    }

    // Check EventBroadcaster path: {"channel": "...", "payload": {"event_type": "...", ...}}
    if let Some(payload) = event.get("payload") {
        if let Some(et) = payload.get("event_type").and_then(|v| v.as_str()) {
            if et.contains("cascade") {
                return true;
            }
        }
        if let Some(data) = payload.get("data") {
            if let Some(et) = data.get("event_type").and_then(|v| v.as_str()) {
                if et.contains("cascade") {
                    return true;
                }
            }
        }
    }

    false
}

/// Push a cascade event into the ring buffer.
async fn push_event(event: Value) {
    let buffer = get_buffer();
    let mut events = buffer.lock().await;
    if events.len() >= MAX_RING_BUFFER_SIZE {
        events.pop_front();
    }
    events.push_back(event);
}

/// GET /cascade/events — Returns recent cascade events as JSON array
async fn cascade_events_handler(State(_state): State<Arc<ApiState>>) -> Json<Value> {
    let buffer = get_buffer();
    let events = buffer.lock().await;
    let snapshot: Vec<Value> = events.iter().cloned().collect();
    let count = snapshot.len();
    Json(serde_json::json!({
        "success": true,
        "data": snapshot,
        "count": count
    }))
}

/// GET /cascade/stream — SSE stream of cascade events with 30s keepalive
async fn cascade_stream_handler(
    State(state): State<Arc<ApiState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let event_rx = state.app_state.event_broadcast.subscribe();

    info!("SSE client connected for cascade event streaming");

    let stream =
        FuturesStreamExt::filter_map(BroadcastStream::new(event_rx), |result| async move {
            match result {
                Ok(event) => {
                    if !is_cascade_event(&event) {
                        return None;
                    }

                    // Note: ring buffer is populated by the background task (spawn_buffer_task),
                    // so we don't push here to avoid duplicates.

                    match serde_json::to_string(&event) {
                        Ok(json_str) => {
                            let event_subtype = event
                                .get("event_type")
                                .or_else(|| event.get("event"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("cascade");

                            Some(Ok(Event::default()
                                .event(format!("cascade/{}", event_subtype))
                                .data(json_str)))
                        }
                        Err(_) => None,
                    }
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    Some(Ok(Event::default().event("cascade/warning").data(format!(
                        "{{\"message\": \"Skipped {} events due to lag\"}}",
                        n
                    ))))
                }
            }
        });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("keepalive"),
    )
}

/// The `worker_supervisor` registry name of the loop [`spawn_buffer_task`]
/// starts, as `GET /health` `supervised_workers` lists it.
pub const CASCADE_BUFFER_WORKER: &str = "cascade.buffer";

/// Start the background task that populates the cascade ring buffer.
///
/// Subscribes to the broadcast channel and filters cascade events into the
/// ring buffer even when no SSE clients are connected. This ensures
/// `/cascade/events` has data available immediately.
///
/// The loop runs under the panic supervisor, so a panic is logged, counted on
/// `/health` and the loop rebuilt after a backoff. Each run subscribes afresh
/// ([`buffer_run`]): a receiver moved into one run dies with it. A closed
/// channel ends the loop on purpose, which `/health` records as `exited`.
pub fn spawn_buffer_task(
    event_broadcast: tokio::sync::broadcast::Sender<Value>,
) -> tokio::task::JoinHandle<()> {
    crate::worker_supervisor::spawn_supervised(CASCADE_BUFFER_WORKER, move || {
        buffer_run(&event_broadcast)
    })
}

/// One run of the buffer loop: subscribes NOW (when the supervisor calls the
/// factory, not when the future is first polled) and returns the loop that
/// drains that receiver into the ring buffer.
fn buffer_run(
    event_broadcast: &tokio::sync::broadcast::Sender<Value>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let mut rx = event_broadcast.subscribe();
    async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if is_cascade_event(&event) {
                        push_event(event).await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Cascade buffer lagged, skipped {} events", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("Cascade buffer: broadcast channel closed");
                    break;
                }
            }
        }
    }
}

/// Create routes for this module.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/cascade/events", get(cascade_events_handler))
        .route("/cascade/stream", get(cascade_stream_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wait until the ring buffer holds an event carrying `marker`.
    async fn buffered(marker: &str) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if get_buffer()
                .lock()
                .await
                .iter()
                .any(|e| e["marker"] == marker)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn cascade_event(marker: &str) -> Value {
        serde_json::json!({ "event_type": "cascade_test", "marker": marker })
    }

    /// What a supervisor restart does: call the factory a second time. Each run
    /// subscribes for itself, so the rebuilt run still receives — a receiver
    /// taken once before the spawn would have died with the first run.
    #[tokio::test(flavor = "multi_thread")]
    async fn each_factory_run_receives_events_broadcast_after_it_subscribes() {
        let (tx, _keep) = tokio::sync::broadcast::channel::<Value>(16);

        let first = tokio::spawn(buffer_run(&tx));
        let marker = format!("first-{}", uuid::Uuid::new_v4());
        tx.send(cascade_event(&marker))
            .expect("a receiver is subscribed");
        assert!(
            buffered(&marker).await,
            "the first run never buffered its event"
        );
        // A panic ends the run; aborting it is the same loss of its receiver.
        first.abort();
        let _ = first.await;

        let second = tokio::spawn(buffer_run(&tx));
        let marker = format!("second-{}", uuid::Uuid::new_v4());
        tx.send(cascade_event(&marker))
            .expect("a receiver is subscribed");
        assert!(
            buffered(&marker).await,
            "the rebuilt run never buffered its event"
        );
        second.abort();
    }

    /// A closed channel ends a run: the loop returns, which the supervisor
    /// records as `exited` rather than restarting it.
    #[tokio::test]
    async fn a_run_returns_when_the_channel_closes() {
        let (tx, _) = tokio::sync::broadcast::channel::<Value>(16);
        let run = buffer_run(&tx);
        drop(tx);
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("the run must return once its channel is closed");
    }

    /// The door `create_router` calls registers the loop with the panic
    /// supervisor under its `/health` name.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_buffer_task_registers_with_the_panic_supervisor() {
        let (tx, _keep) = tokio::sync::broadcast::channel::<Value>(16);
        let handle = spawn_buffer_task(tx.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !crate::worker_supervisor::snapshot()
            .iter()
            .any(|row| row.name == CASCADE_BUFFER_WORKER)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "spawn_buffer_task never registered {CASCADE_BUFFER_WORKER}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.abort();
    }
}
