//! Test support for the scheduler's `require_idle` entry-point test,
//! `scheduler_service::tests::scheduler_idle_gate_fires_only_when_status_data_reads_ready_and_idle`:
//! the stub `GET /status`, the answers it is swapped through, and the
//! idle-gated task. The test itself lives in `scheduler_service.rs`'s inline
//! `mod tests`, because the arming proof cannot derive a libtest path for a
//! child module FILE of a parent that invokes an item-position macro
//! (`tokio::task_local!`); keeping the helpers here keeps that file from
//! growing by the whole matrix.
//!
//! The gate used to read `/status` at the top level, outside its `data`
//! envelope, so it never saw `Ready` and an idle-gated task never fired (coord
//! finding `89c8c10e`). Plan
//! `2026-10-24-runner-idle-gate-reads-status-outside-its-envelope` Phase 2.

use super::*;
use crate::scheduler::{scheduled_task_type_default, IdleCondition, ScheduleConditions};

/// The `/status` writer's idle answer: the `ApiResponse` envelope with
/// `executor_state` `Ready` and no AI analysis running.
pub(super) const READY_AND_IDLE: &str =
    r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":false}}"#;

/// Every answer that must HOLD the task: (case, HTTP status, body).
pub(super) const NOT_IDLE: [(&str, u16, &str); 8] = [
    (
        "Ready with AI analysis running",
        200,
        r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":true}}"#,
    ),
    (
        "executor Running",
        200,
        r#"{"success":true,"data":{"executor_state":"Running","ai_analysis_running":false}}"#,
    ),
    (
        "executor not_started (no bridge)",
        200,
        r#"{"success":true,"data":{"executor_state":"not_started","ai_analysis_running":false}}"#,
    ),
    (
        "ai_analysis_running null",
        200,
        r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":null}}"#,
    ),
    (
        "ai_analysis_running absent",
        200,
        r#"{"success":true,"data":{"executor_state":"Ready"}}"#,
    ),
    (
        "success: false",
        200,
        r#"{"success":false,"error":"Internal error"}"#,
    ),
    ("HTTP 500 with an idle body", 500, READY_AND_IDLE),
    (
        "the old un-enveloped idle shape",
        200,
        r#"{"executor_state":"Ready","ai_analysis_running":false}"#,
    ),
];

/// What the stub `/status` answers: (HTTP status, body). Swapped per case.
pub(super) type StubReply = Arc<std::sync::Mutex<(u16, &'static str)>>;

/// A `/status` stub on an ephemeral loopback port. Returns its port and the
/// handle that sets its answer.
pub(super) async fn spawn_status_stub() -> (u16, StubReply) {
    use axum::{extract::State, http::StatusCode, routing::get, Router};

    async fn status(State(reply): State<StubReply>) -> (StatusCode, &'static str) {
        let (code, body) = *reply.lock().unwrap();
        (StatusCode::from_u16(code).unwrap(), body)
    }

    let reply: StubReply = Arc::new(std::sync::Mutex::new((200, "")));
    let app = Router::new()
        .route("/status", get(status))
        .with_state(reply.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, reply)
}

/// A loopback port that refuses connections for as long as the returned
/// socket lives: bound, never listening, so no parallel test can take it.
pub(super) fn closed_port() -> (u16, tokio::net::TcpSocket) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = socket.local_addr().unwrap().port();
    (port, socket)
}

pub(super) fn idle_gated_task() -> ScheduledTask {
    let mut task = ScheduledTask::new(
        "idle-gated".to_string(),
        None,
        ScheduleExpression::Cron("0 20 7 * * *".to_string()),
        scheduled_task_type_default(),
    );
    task.conditions = Some(ScheduleConditions {
        require_idle: Some(IdleCondition { enabled: true }),
        ..Default::default()
    });
    task
}
