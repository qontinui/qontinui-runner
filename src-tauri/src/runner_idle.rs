//! The runner's idle verdict, read from its own `GET /status`.
//!
//! `GET /status` (`mcp::misc::get_status`) answers an `ApiResponse` envelope:
//! `{"success":true,"data":{"executor_state":…,"ai_analysis_running":…}}`.
//! Its two field readers — the scheduler's `require_idle` gate and the trigger
//! evaluator's `require_idle` condition — used to read both fields at the TOP
//! level, so `executor_state` was always absent and the runner was never idle
//! to either of them (coord finding `89c8c10e-78a0-4e94-9ed2-2db346e512b4`). This
//! module is the one typed reader both gates share, and its tests serialize
//! the WRITER's own `ApiResponse<StatusResponse>`, so a writer-side rename or
//! envelope change fails the build here rather than silently starving every
//! idle-gated schedule and trigger again.
//!
//! **Unknown is NOT idle.** Idle *authorizes* a run, so this is a gating
//! consumer: every input it cannot positively read as idle — a non-2xx, an
//! unparseable body, a `success` that is not `true`, a missing `data` or
//! `executor_state`, an absent or `null` `ai_analysis_running` — withholds the
//! run as [`IdleVerdict::Unknown`] instead of defaulting to idle. The idle
//! definition itself is unchanged: `executor_state == "Ready"` and
//! `ai_analysis_running == false`.
//!
//! [`check_runner_idle`] is the one HTTP read both gates make: one process-wide
//! loopback client, a per-request bound ([`STATUS_PROBE_TIMEOUT`]), and a base
//! URL from [`self_status_base_url`] spelled `127.0.0.1` — the runner binds the
//! IPv4 loopback only, and `localhost` resolves to `::1` first on Windows. A
//! transport failure or a timeout is `Unknown`, never idle.
//!
//! Plan `2026-10-24-runner-idle-gate-reads-status-outside-its-envelope`.

use crate::commands::AppState;
use serde::Deserialize;
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

/// Per-request bound on one `/status` self-probe, for both gates (the trigger
/// reader's long-standing 5 s). The scheduler's reader had none, so a hung
/// `/status` held its serialized tick.
pub const STATUS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect bound for the loopback self-probe: a closed or wedged listener
/// fails here instead of spending the whole request bound.
const STATUS_PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether this runner is idle, as read from its own `GET /status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleVerdict {
    /// A 2xx `success: true` envelope whose `data` reads `executor_state`
    /// `"Ready"` and `ai_analysis_running` `false`.
    Idle,
    /// The status was read and says the runner is working; the reason names
    /// which half of the idle definition failed.
    Busy(String),
    /// The status could not be read as either; the reason names what was
    /// missing. Withholds the run exactly as `Busy` does.
    Unknown(String),
}

impl IdleVerdict {
    /// True only for [`IdleVerdict::Idle`]; `Busy` and `Unknown` both withhold.
    pub fn is_idle(&self) -> bool {
        matches!(self, IdleVerdict::Idle)
    }
}

impl fmt::Display for IdleVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdleVerdict::Idle => f.write_str("idle"),
            IdleVerdict::Busy(reason) => write!(f, "busy ({reason})"),
            IdleVerdict::Unknown(reason) => write!(f, "unknown ({reason})"),
        }
    }
}

/// Reader-side mirror of `ApiResponse<StatusResponse>`. Every field is
/// optional so a missing one becomes `Unknown`, never a serde default.
#[derive(Deserialize)]
struct StatusEnvelope {
    success: Option<bool>,
    data: Option<StatusData>,
}

#[derive(Deserialize)]
struct StatusData {
    executor_state: Option<String>,
    /// `Option` so a `bool` writer and a nullable writer both parse, and so an
    /// absent or `null` value is `None` — never `false`.
    ai_analysis_running: Option<bool>,
}

/// Turn one `GET /status` answer (HTTP status code plus raw body) into an
/// [`IdleVerdict`]. Pure: no I/O, no clock.
pub fn idle_verdict_from_status_body(status: u16, body: &[u8]) -> IdleVerdict {
    if !(200..300).contains(&status) {
        return IdleVerdict::Unknown(format!("http {status}"));
    }
    let envelope: StatusEnvelope = match serde_json::from_slice(body) {
        Ok(envelope) => envelope,
        Err(e) => return IdleVerdict::Unknown(format!("unparseable /status body: {e}")),
    };
    match envelope.success {
        Some(true) => {}
        Some(false) => return IdleVerdict::Unknown("/status reported success: false".into()),
        None => return IdleVerdict::Unknown("/status envelope has no success".into()),
    }
    let Some(data) = envelope.data else {
        return IdleVerdict::Unknown("/status envelope has no data".into());
    };
    let Some(executor_state) = data.executor_state else {
        return IdleVerdict::Unknown("/status data has no executor_state".into());
    };
    let Some(ai_analysis_running) = data.ai_analysis_running else {
        return IdleVerdict::Unknown("/status data.ai_analysis_running is absent or null".into());
    };
    if executor_state != "Ready" {
        return IdleVerdict::Busy(format!("executor {executor_state}"));
    }
    if ai_analysis_running {
        return IdleVerdict::Busy("ai analysis running".into());
    }
    IdleVerdict::Idle
}

/// This runner's own API base for the `/status` self-probe:
/// `http://127.0.0.1:<port>`, the port being the actually-bound one from
/// `AppState` when there is one, else `QONTINUI_PORT` (default 9876) — the
/// `AppState`-less mode both gates have always had.
pub fn self_status_base_url(app_state: Option<&AppState>) -> String {
    let port = match app_state {
        Some(state) => crate::mcp::types::runner_api_port(state),
        None => crate::mcp::types::get_mcp_api_port(),
    };
    format!("http://127.0.0.1:{port}")
}

/// The one process-wide client for the `/status` self-probe, built once from
/// the sanctioned constructor (`outbound_net::os_dns::client_builder`).
///
/// `pool_max_idle_per_host(0)`: a loopback probe gains nothing from pooling,
/// and a pooled idle connection must never outlive the tokio runtime that
/// opened it, which a static client would otherwise allow. `no_proxy()`: a
/// call to this runner itself is never a proxy's to carry. A builder failure
/// is kept, not panicked on — the probe then reads `Unknown`, which withholds
/// the run, rather than killing the tick that asked.
fn status_client() -> Result<&'static reqwest::Client, &'static str> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            qontinui_runner_lib::outbound_net::os_dns::client_builder()
                .connect_timeout(STATUS_PROBE_CONNECT_TIMEOUT)
                .pool_max_idle_per_host(0)
                .no_proxy()
                .build()
                .map_err(|e| format!("reqwest builder: {e}"))
        })
        .as_ref()
        .map_err(String::as_str)
}

/// Read this runner's idle verdict with one `GET {base_url}/status`, bounded
/// by `timeout`. A transport error, a timeout or an unreadable body is
/// [`IdleVerdict::Unknown`]; anything that answered goes through
/// [`idle_verdict_from_status_body`].
pub async fn check_runner_idle(base_url: &str, timeout: Duration) -> IdleVerdict {
    let client = match status_client() {
        Ok(client) => client,
        Err(e) => return IdleVerdict::Unknown(format!("no /status client: {e}")),
    };
    let url = format!("{}/status", base_url.trim_end_matches('/'));
    let response = match client.get(&url).timeout(timeout).send().await {
        Ok(response) => response,
        Err(e) => {
            return IdleVerdict::Unknown(format!(
                "GET {url}: {}",
                crate::util::error_chain::error_chain(&e)
            ))
        }
    };
    let status = response.status().as_u16();
    match response.bytes().await {
        Ok(body) => idle_verdict_from_status_body(status, &body),
        Err(e) => IdleVerdict::Unknown(format!(
            "GET {url}: reading the body: {}",
            crate::util::error_chain::error_chain(&e)
        )),
    }
}

/// How often an `Unknown` verdict is logged at `warn!`, per gate. An `Unknown`
/// withholds every idle-gated task and trigger, and some causes never clear by
/// themselves (a cached client-builder error, a port read as 0, a guarded 503),
/// so it must be visible above `debug!` without warning every scheduler tick.
const UNKNOWN_WARN_INTERVAL: Duration = Duration::from_secs(300);

/// Whether an `Unknown` seen at `now` is due a `warn!`, given when this gate
/// last warned: always on the first, then at most once per `interval`.
fn unknown_warn_due(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    interval: Duration,
) -> bool {
    match last {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= interval,
    }
}

/// Log one gate's verdict: `Unknown` at `warn!` (rate-limited per gate by
/// [`UNKNOWN_WARN_INTERVAL`]), `Idle` / `Busy` at `debug!`.
pub fn log_verdict(gate: &'static str, verdict: &IdleVerdict) {
    static LAST_WARN: std::sync::Mutex<Vec<(&'static str, std::time::Instant)>> =
        std::sync::Mutex::new(Vec::new());
    if matches!(verdict, IdleVerdict::Unknown(_)) {
        let now = std::time::Instant::now();
        let mut last = LAST_WARN.lock().unwrap_or_else(|p| p.into_inner());
        let prev = last.iter().find(|(g, _)| *g == gate).map(|(_, at)| *at);
        if unknown_warn_due(prev, now, UNKNOWN_WARN_INTERVAL) {
            last.retain(|(g, _)| *g != gate);
            last.push((gate, now));
            tracing::warn!("{gate}: runner idle state is {verdict}; idle-gated work is held");
            return;
        }
    }
    tracing::debug!("{gate}: runner is {verdict}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::state::ExecutorState;
    use crate::mcp::misc::StatusResponse;
    use crate::mcp::types::ApiResponse;

    /// The bytes the `/status` WRITER produces for these two fields.
    fn writer_body(executor_state: &str, ai_analysis_running: bool) -> Vec<u8> {
        serde_json::to_vec(&ApiResponse::success(StatusResponse {
            instance_name: None,
            api_port: 9876,
            executor_running: true,
            executor_state: executor_state.to_string(),
            config_loaded: true,
            config_path: None,
            ai_analysis_running,
        }))
        .expect("StatusResponse serializes")
    }

    fn is_unknown(verdict: &IdleVerdict) -> bool {
        matches!(verdict, IdleVerdict::Unknown(_))
    }

    // ---- contract round-trip: the writer's own type through the reader ----

    #[test]
    fn contract_writer_ready_and_no_ai_is_idle() {
        let verdict = idle_verdict_from_status_body(200, &writer_body("Ready", false));
        assert_eq!(verdict, IdleVerdict::Idle);
        assert!(verdict.is_idle());
    }

    #[test]
    fn contract_writer_ai_analysis_running_is_busy() {
        assert_eq!(
            idle_verdict_from_status_body(200, &writer_body("Ready", true)),
            IdleVerdict::Busy("ai analysis running".to_string())
        );
    }

    #[test]
    fn contract_writer_running_executor_is_busy() {
        assert_eq!(
            idle_verdict_from_status_body(200, &writer_body("Running", false)),
            IdleVerdict::Busy("executor Running".to_string())
        );
    }

    /// Every `ExecutorState` the writer can name, through `name()` exactly as
    /// `get_status` calls it. The match has no wildcard, so a new variant
    /// fails to compile here until someone decides whether it is idle.
    #[test]
    fn contract_every_executor_state_name_only_ready_is_idle() {
        let states = [
            ExecutorState::initializing(),
            ExecutorState::ready(),
            ExecutorState::running(),
            ExecutorState::failed("boom".to_string()),
            ExecutorState::shutdown(),
        ];
        for state in states {
            let expect_idle = match state {
                ExecutorState::Ready { .. } => true,
                ExecutorState::Initializing { .. }
                | ExecutorState::Running { .. }
                | ExecutorState::Failed { .. }
                | ExecutorState::Shutdown { .. } => false,
            };
            let verdict = idle_verdict_from_status_body(200, &writer_body(state.name(), false));
            assert_eq!(verdict.is_idle(), expect_idle, "{state:?} -> {verdict}");
        }
    }

    // ---- enumerated shapes, as literal bodies ----

    #[test]
    fn ready_and_ai_false_is_idle() {
        let body =
            br#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":false}}"#;
        assert_eq!(idle_verdict_from_status_body(200, body), IdleVerdict::Idle);
    }

    #[test]
    fn every_non_ready_executor_state_is_busy() {
        let cases: [(&[u8], &str); 6] = [
            (
                br#"{"success":true,"data":{"executor_state":"Initializing","ai_analysis_running":false}}"#,
                "executor Initializing",
            ),
            (
                br#"{"success":true,"data":{"executor_state":"Running","ai_analysis_running":false}}"#,
                "executor Running",
            ),
            (
                br#"{"success":true,"data":{"executor_state":"Failed","ai_analysis_running":false}}"#,
                "executor Failed",
            ),
            (
                br#"{"success":true,"data":{"executor_state":"Shutdown","ai_analysis_running":false}}"#,
                "executor Shutdown",
            ),
            (
                br#"{"success":true,"data":{"executor_state":"not_started","ai_analysis_running":false}}"#,
                "executor not_started",
            ),
            (
                br#"{"success":true,"data":{"executor_state":"Hibernating","ai_analysis_running":false}}"#,
                "executor Hibernating",
            ),
        ];
        for (body, reason) in cases {
            assert_eq!(
                idle_verdict_from_status_body(200, body),
                IdleVerdict::Busy(reason.to_string()),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn ai_analysis_running_true_is_busy() {
        let body =
            br#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":true}}"#;
        assert_eq!(
            idle_verdict_from_status_body(200, body),
            IdleVerdict::Busy("ai analysis running".to_string())
        );
    }

    #[test]
    fn ai_analysis_running_null_is_unknown_not_idle() {
        let body =
            br#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":null}}"#;
        assert_eq!(
            idle_verdict_from_status_body(200, body),
            IdleVerdict::Unknown("/status data.ai_analysis_running is absent or null".to_string())
        );
    }

    #[test]
    fn ai_analysis_running_absent_is_unknown_not_idle() {
        let body = br#"{"success":true,"data":{"executor_state":"Ready"}}"#;
        assert_eq!(
            idle_verdict_from_status_body(200, body),
            IdleVerdict::Unknown("/status data.ai_analysis_running is absent or null".to_string())
        );
    }

    #[test]
    fn executor_state_absent_or_null_is_unknown() {
        for body in [
            &br#"{"success":true,"data":{"ai_analysis_running":false}}"#[..],
            &br#"{"success":true,"data":{"executor_state":null,"ai_analysis_running":false}}"#[..],
        ] {
            assert_eq!(
                idle_verdict_from_status_body(200, body),
                IdleVerdict::Unknown("/status data has no executor_state".to_string()),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn success_false_is_unknown_even_with_idle_data() {
        for body in [
            &br#"{"success":false,"error":"Internal error: join failed"}"#[..],
            &br#"{"success":false,"data":{"executor_state":"Ready","ai_analysis_running":false}}"#
                [..],
        ] {
            assert_eq!(
                idle_verdict_from_status_body(200, body),
                IdleVerdict::Unknown("/status reported success: false".to_string()),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn success_absent_is_unknown_even_with_idle_data() {
        let body = br#"{"data":{"executor_state":"Ready","ai_analysis_running":false}}"#;
        assert_eq!(
            idle_verdict_from_status_body(200, body),
            IdleVerdict::Unknown("/status envelope has no success".to_string())
        );
    }

    #[test]
    fn data_absent_or_null_is_unknown() {
        for body in [
            &br#"{"success":true}"#[..],
            &br#"{"success":true,"data":null}"#[..],
        ] {
            assert_eq!(
                idle_verdict_from_status_body(200, body),
                IdleVerdict::Unknown("/status envelope has no data".to_string()),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    /// The shape both pre-fix readers assumed: no envelope, fields at the top.
    #[test]
    fn old_unenveloped_top_level_shape_is_not_idle() {
        let body = br#"{"executor_state":"Ready","ai_analysis_running":false}"#;
        let verdict = idle_verdict_from_status_body(200, body);
        assert!(!verdict.is_idle(), "{verdict}");
        assert!(is_unknown(&verdict), "{verdict}");
    }

    #[test]
    fn non_2xx_with_an_idle_body_is_unknown() {
        let body =
            br#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":false}}"#;
        assert_eq!(
            idle_verdict_from_status_body(500, body),
            IdleVerdict::Unknown("http 500".to_string())
        );
        assert_eq!(
            idle_verdict_from_status_body(404, body),
            IdleVerdict::Unknown("http 404".to_string())
        );
    }

    #[test]
    fn empty_and_non_json_bodies_are_unknown() {
        for body in [
            &b""[..],
            &b"<html>502 Bad Gateway</html>"[..],
            &b"null"[..],
            &b"[]"[..],
        ] {
            let verdict = idle_verdict_from_status_body(200, body);
            assert!(
                is_unknown(&verdict),
                "{} -> {verdict}",
                String::from_utf8_lossy(body)
            );
        }
    }

    // ---- the HTTP read: check_runner_idle against a stub /status ----

    const IDLE_BODY: &str =
        r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":false}}"#;

    /// A `/status` stub on an ephemeral loopback port that answers 200 with
    /// `body` after `delay`. Returns its base URL.
    async fn spawn_status_stub(body: &'static str, delay: Duration) -> String {
        use axum::{routing::get, Router};
        let app = Router::new().route(
            "/status",
            get(move || async move {
                tokio::time::sleep(delay).await;
                body
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://127.0.0.1:{port}")
    }

    /// A loopback base URL that refuses connections for as long as the returned
    /// socket lives: bound, never listening, so no parallel test can take the
    /// port and answer on it meanwhile.
    fn closed_base_url() -> (String, tokio::net::TcpSocket) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        (format!("http://127.0.0.1:{port}"), socket)
    }

    #[tokio::test]
    async fn check_runner_idle_reads_an_idle_status_as_idle() {
        let base = spawn_status_stub(IDLE_BODY, Duration::ZERO).await;
        assert_eq!(
            check_runner_idle(&base, STATUS_PROBE_TIMEOUT).await,
            IdleVerdict::Idle
        );
    }

    /// The bound is the request's own: a `/status` that answers IDLE, but only
    /// after 1 s, read with a 200 ms bound, is `Unknown` — a reader that
    /// waited for the late answer would read it as idle.
    #[tokio::test]
    async fn check_runner_idle_times_out_to_unknown() {
        let base = spawn_status_stub(IDLE_BODY, Duration::from_secs(1)).await;
        let verdict = check_runner_idle(&base, Duration::from_millis(200)).await;
        assert!(is_unknown(&verdict), "{verdict}");
    }

    #[tokio::test]
    async fn check_runner_idle_on_a_closed_port_is_unknown() {
        let (base, _held) = closed_base_url();
        let verdict = check_runner_idle(&base, STATUS_PROBE_TIMEOUT).await;
        assert!(is_unknown(&verdict), "{verdict}");
    }

    /// Without an `AppState` the base is `QONTINUI_PORT` on the IPv4 loopback,
    /// never `localhost`.
    #[test]
    fn self_status_base_url_without_app_state_is_ipv4_loopback_on_qontinui_port() {
        let _lock = crate::test_env::env_lock();
        let _env = crate::test_env::EnvVarRestore::capture(&["QONTINUI_PORT"]);
        std::env::set_var("QONTINUI_PORT", "45678");
        assert_eq!(self_status_base_url(None), "http://127.0.0.1:45678");
    }

    #[test]
    fn unknown_warns_first_then_once_per_interval() {
        let t0 = std::time::Instant::now();
        let every = Duration::from_secs(300);
        assert!(
            unknown_warn_due(None, t0, every),
            "the first Unknown must warn"
        );
        assert!(!unknown_warn_due(
            Some(t0),
            t0 + Duration::from_secs(299),
            every
        ));
        assert!(unknown_warn_due(
            Some(t0),
            t0 + Duration::from_secs(300),
            every
        ));
    }

    #[test]
    fn is_idle_and_display() {
        assert!(IdleVerdict::Idle.is_idle());
        assert!(!IdleVerdict::Busy("executor Running".to_string()).is_idle());
        assert!(!IdleVerdict::Unknown("http 500".to_string()).is_idle());
        assert_eq!(IdleVerdict::Idle.to_string(), "idle");
        assert_eq!(
            IdleVerdict::Busy("ai analysis running".to_string()).to_string(),
            "busy (ai analysis running)"
        );
        assert_eq!(
            IdleVerdict::Unknown("http 404".to_string()).to_string(),
            "unknown (http 404)"
        );
    }
}
