//! Typed permission requests on the Claude structured lane — the runner's
//! answer to every `control_request` the CLI sends (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 9).
//!
//! Before this module every inbound control request was answered
//! `{"allowed": true}` whatever its subtype. Claude Code 2.1.285 silently
//! ignores that shape and waits forever (Phase 2 probe Q1,
//! `tests/fixtures/cli_protocol/claude/2.1.285/can_use_tool_runner_shape_ignored`),
//! so it was a latent hang that only bypass mode kept unreachable. Now each
//! request is answered BY SUBTYPE, in the SDK's nested shape
//! ([`OutgoingControlResponse`]):
//!
//! | Request | [`PermissionMode`] | Answer |
//! |---|---|---|
//! | `can_use_tool` | a bypass mode | allow, `updatedInput` = the request's own input |
//! | `can_use_tool` | [`PermissionMode::Prompt`] | parked; the operator answers through [`PermissionBroker::respond`] |
//! | `can_use_tool`, malformed | any | `error` naming what is missing |
//! | any other subtype | any | `error`: `unsupported control request subtype: <x>` — never a fabricated allow |
//!
//! ## A parked request is never left hanging
//!
//! A `Prompt`-mode request nobody answers would stall the turn for good (plan
//! Risks, "A permission prompt nobody is watching"). Each parked request
//! therefore carries a decision bound ([`decision_timeout`], 10 minutes by
//! default); when it lapses the runner DENIES the call — fail closed — with a
//! message naming the bound, and announces it.
//!
//! **Why a timeout is a deny and not a `SessionFailure`.** Nothing failed: the
//! CLI treats a deny as an ordinary error `tool_result`, hands it to the model
//! and continues the same turn, which still ends `is_error: false` (probe Q1,
//! `can_use_tool_sdk_deny`). A `SessionFailure` would tell every surface the
//! session is broken and select a recovery policy, and no policy fits a session
//! that is healthy and still working. So a timeout is surfaced as what it is —
//! a resolution with outcome [`PermissionOutcome::TimedOut`] on the
//! `session-permission-resolved` event, plus a status line in the conversation.
//!
//! The pending set is readable without having been subscribed: the Tauri
//! command `session_pending_permissions` and `GET
//! /sessions/{id}/permission-requests` both call [`PermissionBroker::pending`].

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::claude_protocol::types::{CliControlRequest, OutgoingControlResponse};
use crate::session::launch_spec::PermissionMode;

use super::writer::StdinWriter;

/// Tauri (and WS) event announcing a parked permission request.
pub const PERMISSION_REQUEST_EVENT: &str = "session-permission-request";
/// Tauri (and WS) event announcing that a parked request was answered, timed
/// out, or ended with its session.
pub const PERMISSION_RESOLVED_EVENT: &str = "session-permission-resolved";

/// Environment override of the decision bound, in seconds.
pub const DECISION_TIMEOUT_ENV: &str = "QONTINUI_PERMISSION_DECISION_TIMEOUT_SECS";
/// The decision bound when neither the launch nor the environment sets one.
pub const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(600);
/// The shortest bound a launch may ask for: an operator needs time to read.
pub const MIN_DECISION_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest bound a launch may ask for.
pub const MAX_DECISION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// The message the model receives for a deny the operator gave no reason for.
pub const DEFAULT_DENY_MESSAGE: &str = "The operator denied this tool call.";

/// The decision bound for a prompted session: the launch's `requested_secs`,
/// else [`DECISION_TIMEOUT_ENV`], else [`DEFAULT_DECISION_TIMEOUT`] — clamped
/// to [`MIN_DECISION_TIMEOUT`]..=[`MAX_DECISION_TIMEOUT`]. An unparseable
/// environment value is ignored with a warning, never read as zero.
pub fn decision_timeout(requested_secs: Option<u64>) -> Duration {
    let from_env = || {
        let raw = std::env::var(DECISION_TIMEOUT_ENV).ok()?;
        match raw.trim().parse::<u64>() {
            Ok(secs) => Some(secs),
            Err(e) => {
                warn!("{DECISION_TIMEOUT_ENV}={raw:?} is not a number of seconds ({e}); using the default");
                None
            }
        }
    };
    requested_secs
        .or_else(from_env)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_DECISION_TIMEOUT)
        .clamp(MIN_DECISION_TIMEOUT, MAX_DECISION_TIMEOUT)
}

/// Where a control response is written — the session's stdin in production.
pub trait ControlResponder: Send + Sync {
    fn respond(&self, response: &OutgoingControlResponse) -> Result<(), String>;
}

impl ControlResponder for StdinWriter {
    fn respond(&self, response: &OutgoingControlResponse) -> Result<(), String> {
        self.write_message(response)
    }
}

/// Where permission notices go. Production is [`TauriPermissionSink`].
pub trait PermissionSink: Send + Sync {
    fn requested(&self, notice: &PermissionRequestNotice);
    fn resolved(&self, notice: &PermissionResolvedNotice);
}

/// One parked `can_use_tool` request — the `session-permission-request`
/// payload and one row of [`PermissionBroker::pending`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequestNotice {
    /// The runner session (SessionManager key, == the task run id).
    pub session_id: String,
    /// The CLI's request id; the operator's answer names it.
    pub request_id: String,
    pub tool_name: String,
    pub display_name: Option<String>,
    /// The CLI's one-line description of the call (e.g. the target file).
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    /// The tool's arguments, as the CLI sent them.
    pub input: Value,
    /// The CLI's `permission_suggestions` (rule changes it offers).
    pub suggestions: Vec<Value>,
    /// RFC 3339.
    pub requested_at: String,
    /// RFC 3339 — when the runner will deny it unanswered.
    pub expires_at: String,
    pub timeout_secs: u64,
}

/// How a parked request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOutcome {
    /// The operator allowed it.
    Allowed,
    /// The operator denied it.
    Denied,
    /// Nobody decided within the bound; the runner denied it.
    TimedOut,
    /// The session ended first; nothing was sent (there is no CLI to tell).
    SessionEnded,
    /// The CLI sent a NEW request under the same id while this one was still
    /// parked. Nothing is sent for the old one — an answer naming that id
    /// would be read as the answer to the new request — but it is resolved,
    /// so no surface keeps live buttons for it.
    Superseded,
}

/// The `session-permission-resolved` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionResolvedNotice {
    pub session_id: String,
    pub request_id: String,
    pub tool_name: String,
    pub outcome: PermissionOutcome,
    /// The deny message the model received, or why the request ended.
    pub message: Option<String>,
    /// A deny that also interrupts the turn.
    pub interrupt: bool,
}

/// The operator's answer to a parked request.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    /// Run the tool. `updated_input` replaces the arguments when the operator
    /// edited them; `None` runs it with the arguments the CLI asked for.
    Allow { updated_input: Option<Value> },
    /// Refuse the call. `message` reaches the model as the call's error
    /// result ([`DEFAULT_DENY_MESSAGE`] when `None`). `interrupt` also stops
    /// the turn — the caller sends the session's interrupt after the deny.
    Deny {
        message: Option<String>,
        interrupt: bool,
    },
}

/// What [`PermissionBroker::handle_control_request`] did with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlHandling {
    /// A bypass-mode `can_use_tool`, allowed.
    Allowed,
    /// A prompt-mode `can_use_tool`, parked for the operator.
    Parked,
    /// Answered with an `error` control response (unsupported subtype or a
    /// malformed request).
    Refused(String),
    /// The request carried no `request_id`, so no answer can name it.
    Unanswerable,
}

struct Parked {
    notice: PermissionRequestNotice,
    /// Dropping or sending wakes the request's timeout thread early.
    cancel: mpsc::Sender<()>,
    /// Which parking this is. A timer expires only ITS parking: a request id
    /// the CLI reuses gets a new generation, so the superseded request's
    /// timer — already past `recv_timeout` when the cancel arrives — cannot
    /// deny the new request.
    generation: u64,
}

/// One structured session's control-request answerer and its parked
/// permission requests. Shared by the session (operator answers, pending
/// reads, close) and its stdout reader (incoming requests).
pub struct PermissionBroker {
    session_id: String,
    mode: PermissionMode,
    timeout: Duration,
    responder: Arc<dyn ControlResponder>,
    sink: Arc<dyn PermissionSink>,
    pending: Mutex<HashMap<String, Parked>>,
    /// Source of [`Parked::generation`].
    next_generation: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for PermissionBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionBroker")
            .field("session_id", &self.session_id)
            .field("mode", &self.mode)
            .field("timeout", &self.timeout)
            .field("pending", &self.pending_count())
            .finish()
    }
}

impl PermissionBroker {
    /// `timeout` is used as given — [`decision_timeout`] is where a launch's
    /// request is bounded.
    pub fn new(
        session_id: impl Into<String>,
        mode: PermissionMode,
        timeout: Duration,
        responder: Arc<dyn ControlResponder>,
        sink: Arc<dyn PermissionSink>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.into(),
            mode,
            timeout,
            responder,
            sink,
            pending: Mutex::new(HashMap::new()),
            next_generation: std::sync::atomic::AtomicU64::new(1),
        })
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Parked>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pending_count(&self) -> usize {
        self.lock().len()
    }

    /// Send one control response. A failure is logged AND returned: a caller
    /// that reports what it answered must not report an answer the CLI never
    /// received.
    fn write(&self, response: &OutgoingControlResponse) -> Result<(), String> {
        self.responder.respond(response).map_err(|e| {
            warn!(session = %self.session_id, "failed to send control response: {e}");
            format!("the answer could not be sent to the CLI: {e}")
        })
    }

    /// Answer — or park — one control request from the CLI. Logs the subtype,
    /// request id and tool name only: a request's input can carry file
    /// contents and commands.
    pub fn handle_control_request(self: &Arc<Self>, req: &CliControlRequest) -> ControlHandling {
        let subtype = req.request.subtype.as_str();
        let Some(request_id) = req.request_id.as_deref() else {
            warn!(session = %self.session_id, subtype, "control request without a request_id; no answer can name it");
            return ControlHandling::Unanswerable;
        };
        let tool = match req.request.as_can_use_tool() {
            None => {
                let error = format!("unsupported control request subtype: {subtype}");
                warn!(session = %self.session_id, request_id, "{error}");
                // A failed write is already logged; the CLI stays unanswered
                // either way and its own timeout applies.
                let _ = self.write(&OutgoingControlResponse::error(request_id, &error));
                return ControlHandling::Refused(error);
            }
            Some(Err(error)) => {
                warn!(session = %self.session_id, request_id, "{error}");
                let _ = self.write(&OutgoingControlResponse::error(request_id, &error));
                return ControlHandling::Refused(error);
            }
            Some(Ok(tool)) => tool,
        };
        if !self.mode.prompts() {
            let _ = self.write(&OutgoingControlResponse::allow_tool_use(
                request_id, tool.input,
            ));
            return ControlHandling::Allowed;
        }

        let now = chrono::Utc::now();
        let expires = now
            + chrono::Duration::from_std(self.timeout).unwrap_or_else(|_| chrono::Duration::zero());
        let notice = PermissionRequestNotice {
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            tool_name: tool.tool_name,
            display_name: tool.display_name,
            description: tool.description,
            tool_use_id: tool.tool_use_id,
            input: tool.input,
            suggestions: tool.permission_suggestions,
            requested_at: now.to_rfc3339(),
            expires_at: expires.to_rfc3339(),
            timeout_secs: self.timeout.as_secs(),
        };
        let (cancel, cancelled) = mpsc::channel::<()>();
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let replaced = self.lock().insert(
            request_id.to_string(),
            Parked {
                notice: notice.clone(),
                cancel,
                generation,
            },
        );
        if let Some(previous) = replaced {
            // Never silently dropped: its timer is stopped and every surface
            // hears it is over. No answer is sent for it — the id now names
            // the new request.
            let _ = previous.cancel.send(());
            warn!(
                session = %self.session_id,
                request_id,
                "a new permission request reused a parked request's id; the old one is superseded"
            );
            self.sink.resolved(&PermissionResolvedNotice {
                session_id: self.session_id.clone(),
                request_id: request_id.to_string(),
                tool_name: previous.notice.tool_name,
                outcome: PermissionOutcome::Superseded,
                message: Some(
                    "The CLI sent a new request under the same id, so this one was withdrawn unanswered."
                        .to_string(),
                ),
                interrupt: false,
            });
        }
        info!(
            session = %self.session_id,
            request_id,
            tool = %notice.tool_name,
            timeout_secs = notice.timeout_secs,
            "permission request parked for the operator"
        );
        self.sink.requested(&notice);
        self.spawn_timeout(request_id, generation, cancelled);
        ControlHandling::Parked
    }

    /// Wait out the bound on a separate thread; deny if still parked. The
    /// thread holds only a weak reference, so a dropped session ends it.
    fn spawn_timeout(
        self: &Arc<Self>,
        request_id: &str,
        generation: u64,
        cancelled: mpsc::Receiver<()>,
    ) {
        let broker: Weak<Self> = Arc::downgrade(self);
        let timeout = self.timeout;
        let id = request_id.to_string();
        let spawned = std::thread::Builder::new()
            .name("permission-timeout".to_string())
            .spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) = cancelled.recv_timeout(timeout) {
                    if let Some(broker) = broker.upgrade() {
                        broker.expire(&id, generation);
                    }
                }
            });
        if let Err(e) = spawned {
            // Without a timer the request could wait forever: deny it now.
            warn!(session = %self.session_id, "could not start the permission timeout thread ({e}); denying now");
            self.expire_with(
                request_id,
                generation,
                "The runner could not start the permission timer, so it denied this request (fail closed).",
            );
        }
    }

    fn expire(&self, request_id: &str, generation: u64) {
        let message = format!(
            "No permission decision within {}s — the runner denied this request (fail closed).",
            self.timeout.as_secs()
        );
        self.expire_with(request_id, generation, &message);
    }

    fn expire_with(&self, request_id: &str, generation: u64, message: &str) {
        let parked = {
            let mut pending = self.lock();
            // Only THIS parking: answered in the meantime, or superseded by a
            // request reusing the id, leaves nothing for this timer to do.
            if pending.get(request_id).map(|p| p.generation) != Some(generation) {
                return;
            }
            pending.remove(request_id)
        };
        let Some(parked) = parked else {
            return;
        };
        warn!(
            session = %self.session_id,
            request_id = %request_id,
            tool = %parked.notice.tool_name,
            "permission request timed out; denied"
        );
        let _ = self.write(&OutgoingControlResponse::deny_tool_use(request_id, message));
        self.sink.resolved(&PermissionResolvedNotice {
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            tool_name: parked.notice.tool_name,
            outcome: PermissionOutcome::TimedOut,
            message: Some(message.to_string()),
            interrupt: false,
        });
    }

    /// The operator's answer to parked request `request_id`. Errors when it is
    /// not parked (already answered, timed out, or never asked) or when an
    /// edited input is not a JSON object. The caller sends the interrupt for
    /// a deny that asks for one (`resolved.interrupt`).
    pub fn respond(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<PermissionResolvedNotice, String> {
        if let PermissionDecision::Allow {
            updated_input: Some(input),
        } = &decision
        {
            if !input.is_object() {
                return Err("updatedInput must be a JSON object of tool arguments".to_string());
            }
        }
        // The pending set stays locked across the write, so the request's
        // timer cannot deny it concurrently, and it is removed only once its
        // answer is actually sent. A failed send leaves it parked (its timer
        // still running) and is an error — the operator is never told an
        // unsent answer was given.
        let mut pending = self.lock();
        let notice = pending
            .get(request_id)
            .map(|p| p.notice.clone())
            .ok_or_else(|| {
                format!(
                    "no pending permission request {request_id} on session {} (already answered, timed out, or never asked)",
                    self.session_id
                )
            })?;
        let resolved = match decision {
            PermissionDecision::Allow { updated_input } => {
                self.write(&OutgoingControlResponse::allow_tool_use(
                    request_id,
                    updated_input.unwrap_or(notice.input),
                ))?;
                PermissionResolvedNotice {
                    session_id: self.session_id.clone(),
                    request_id: request_id.to_string(),
                    tool_name: notice.tool_name,
                    outcome: PermissionOutcome::Allowed,
                    message: None,
                    interrupt: false,
                }
            }
            PermissionDecision::Deny { message, interrupt } => {
                let message = message
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| DEFAULT_DENY_MESSAGE.to_string());
                self.write(&OutgoingControlResponse::deny_tool_use(
                    request_id, &message,
                ))?;
                PermissionResolvedNotice {
                    session_id: self.session_id.clone(),
                    request_id: request_id.to_string(),
                    tool_name: notice.tool_name,
                    outcome: PermissionOutcome::Denied,
                    message: Some(message),
                    interrupt,
                }
            }
        };
        if let Some(parked) = pending.remove(request_id) {
            let _ = parked.cancel.send(());
        }
        drop(pending);
        info!(
            session = %self.session_id,
            request_id,
            outcome = ?resolved.outcome,
            interrupt = resolved.interrupt,
            "permission request answered by the operator"
        );
        self.sink.resolved(&resolved);
        Ok(resolved)
    }

    /// The requests parked now, oldest first.
    pub fn pending(&self) -> Vec<PermissionRequestNotice> {
        let mut out: Vec<_> = self.lock().values().map(|p| p.notice.clone()).collect();
        out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        out
    }

    /// The session's CLI is gone: nothing can be answered any more. Every
    /// parked request is dropped (its timer wakes and exits) and announced as
    /// [`PermissionOutcome::SessionEnded`], so no surface keeps offering
    /// buttons that can do nothing. Idempotent.
    pub fn end_session(&self, reason: &str) {
        let drained: Vec<Parked> = self.lock().drain().map(|(_, p)| p).collect();
        for parked in drained {
            let _ = parked.cancel.send(());
            self.sink.resolved(&PermissionResolvedNotice {
                session_id: self.session_id.clone(),
                request_id: parked.notice.request_id,
                tool_name: parked.notice.tool_name,
                outcome: PermissionOutcome::SessionEnded,
                message: Some(reason.to_string()),
                interrupt: false,
            });
        }
    }
}

/// The production sink: the Tauri event plus the WS re-broadcast (the pair
/// `session::failure_recovery::TauriFailureSink` sends, so a remote or headless
/// consumer hears it too), and a status line in the session's conversation.
pub struct TauriPermissionSink {
    app: tauri::AppHandle,
    session_ctx: Option<crate::mcp::shared::AiSessionContext>,
}

impl TauriPermissionSink {
    pub fn new(
        app: tauri::AppHandle,
        session_ctx: Option<crate::mcp::shared::AiSessionContext>,
    ) -> Self {
        Self { app, session_ctx }
    }

    fn announce<T: Serialize>(&self, event: &str, payload: &T) {
        use tauri::Emitter;
        if let Err(e) = self.app.emit(event, payload) {
            warn!("failed to emit {event}: {e}");
        }
        if crate::event_system::ws_notification_has_receivers(&self.app) {
            match serde_json::to_value(payload) {
                Ok(v) => crate::event_system::broadcast_ws_notification(&self.app, event, &v),
                Err(e) => warn!("{event} payload did not serialize: {e}"),
            }
        }
    }

    fn status_line(&self, text: &str) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::mcp::shared::emit_ai_output(
                &self.app,
                text,
                "status",
                None,
                self.session_ctx.as_ref(),
            );
        }));
    }
}

impl PermissionSink for TauriPermissionSink {
    fn requested(&self, notice: &PermissionRequestNotice) {
        self.announce(PERMISSION_REQUEST_EVENT, notice);
        self.status_line(&format!(
            "Waiting for your permission to use {} (denied automatically after {}s).",
            notice.tool_name, notice.timeout_secs
        ));
    }

    fn resolved(&self, notice: &PermissionResolvedNotice) {
        self.announce(PERMISSION_RESOLVED_EVENT, notice);
        let text = match notice.outcome {
            PermissionOutcome::Allowed => format!("Permission granted: {}.", notice.tool_name),
            PermissionOutcome::Denied if notice.interrupt => {
                format!(
                    "Permission denied: {} — interrupting the turn.",
                    notice.tool_name
                )
            }
            PermissionOutcome::Denied => format!("Permission denied: {}.", notice.tool_name),
            PermissionOutcome::TimedOut => format!(
                "Permission request for {} timed out — {}",
                notice.tool_name,
                notice.message.as_deref().unwrap_or("denied (fail closed).")
            ),
            PermissionOutcome::Superseded => format!(
                "Permission request for {} was replaced by a new request from the CLI.",
                notice.tool_name
            ),
            PermissionOutcome::SessionEnded => return,
        };
        self.status_line(&text);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Recording doubles for the broker's two outputs.

    use super::*;

    #[derive(Default)]
    pub struct RecordingResponder {
        pub sent: Mutex<Vec<String>>,
    }

    impl RecordingResponder {
        pub fn lines(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl ControlResponder for RecordingResponder {
        fn respond(&self, response: &OutgoingControlResponse) -> Result<(), String> {
            let line = crate::claude_protocol::codec::encode_message(response)?;
            self.sent.lock().unwrap().push(line.trim_end().to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    pub struct RecordingSink {
        pub requested: Mutex<Vec<PermissionRequestNotice>>,
        pub resolved: Mutex<Vec<PermissionResolvedNotice>>,
    }

    impl PermissionSink for RecordingSink {
        fn requested(&self, notice: &PermissionRequestNotice) {
            self.requested.lock().unwrap().push(notice.clone());
        }
        fn resolved(&self, notice: &PermissionResolvedNotice) {
            self.resolved.lock().unwrap().push(notice.clone());
        }
    }

    pub fn broker(
        mode: PermissionMode,
        timeout: Duration,
    ) -> (
        Arc<PermissionBroker>,
        Arc<RecordingResponder>,
        Arc<RecordingSink>,
    ) {
        let responder = Arc::new(RecordingResponder::default());
        let sink = Arc::new(RecordingSink::default());
        let broker = PermissionBroker::new(
            "permission-test-session",
            mode,
            timeout,
            responder.clone(),
            sink.clone(),
        );
        (broker, responder, sink)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::broker;
    use super::*;
    use crate::claude_protocol::codec::decode_message;

    fn request(line: &str) -> CliControlRequest {
        decode_message(line)
            .unwrap()
            .as_control_request()
            .cloned()
            .expect("a control_request")
    }

    const WRITE_REQ: &str = r#"{"type":"control_request","request_id":"req-9","request":{"subtype":"can_use_tool","tool_name":"Write","display_name":"Write","input":{"file_path":"/w/probe.txt","content":"hello"},"description":"probe.txt","permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}],"tool_use_id":"toolu_1"}}"#;

    /// A responder whose sends fail (the CLI's stdin is gone).
    struct BrokenStdin;
    impl ControlResponder for BrokenStdin {
        fn respond(&self, _: &OutgoingControlResponse) -> Result<(), String> {
            Err("broken pipe".into())
        }
    }

    /// An answer that could not be SENT is an error, never a reported
    /// Allowed/Denied — and the request stays parked, still answerable (or
    /// still bounded by its timer).
    #[test]
    fn an_unsent_answer_is_an_error_and_the_request_stays_parked() {
        let sink = Arc::new(super::test_support::RecordingSink::default());
        let b = PermissionBroker::new(
            "permission-test-broken",
            PermissionMode::Prompt,
            Duration::from_secs(60),
            Arc::new(BrokenStdin),
            sink.clone(),
        );
        assert_eq!(
            b.handle_control_request(&request(WRITE_REQ)),
            ControlHandling::Parked
        );
        for decision in [
            PermissionDecision::Allow {
                updated_input: None,
            },
            PermissionDecision::Deny {
                message: None,
                interrupt: false,
            },
        ] {
            let err = b.respond("req-9", decision).unwrap_err();
            assert!(err.contains("broken pipe"), "{err}");
        }
        assert!(
            sink.resolved.lock().unwrap().is_empty(),
            "no outcome is announced for an answer the CLI never got"
        );
        assert_eq!(b.pending().len(), 1, "still parked");
    }

    /// A new request reusing a parked request's id resolves the old one
    /// (superseded, nothing sent for it) instead of silently replacing it.
    #[test]
    fn a_reused_request_id_supersedes_the_parked_request() {
        let (b, out, sink) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        b.handle_control_request(&request(WRITE_REQ));
        let again = WRITE_REQ.replace("\"tool_name\":\"Write\"", "\"tool_name\":\"Edit\"");
        assert_eq!(
            b.handle_control_request(&request(&again)),
            ControlHandling::Parked
        );
        let resolved = sink.resolved.lock().unwrap().clone();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].outcome, PermissionOutcome::Superseded);
        assert_eq!(resolved[0].tool_name, "Write");
        assert!(out.lines().is_empty(), "no answer names the reused id");
        let pending = b.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, "Edit");
    }

    /// The superseded request's timer, already past its wait when the new
    /// request (same id) is parked, must not deny the NEW request.
    #[test]
    fn a_superseded_requests_timer_does_not_expire_the_reused_id() {
        let (b, out, _sink) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        b.handle_control_request(&request(WRITE_REQ));
        let first = b.lock().get("req-9").unwrap().generation;
        b.handle_control_request(&request(WRITE_REQ));
        let second = b.lock().get("req-9").unwrap().generation;
        assert_ne!(first, second);

        b.expire("req-9", first); // the old timer firing late
        assert_eq!(b.pending().len(), 1, "the new request is still parked");
        assert!(out.lines().is_empty(), "nothing denied");

        b.expire("req-9", second);
        assert!(b.pending().is_empty());
        assert_eq!(out.lines().len(), 1, "its own timer denies it");
    }

    #[test]
    fn decision_timeout_defaults_bounds_and_honours_a_request() {
        assert_eq!(decision_timeout(Some(120)), Duration::from_secs(120));
        assert_eq!(decision_timeout(Some(1)), MIN_DECISION_TIMEOUT);
        assert_eq!(decision_timeout(Some(u64::MAX / 2)), MAX_DECISION_TIMEOUT);
        // No request: the env override or the default — never zero.
        assert!(decision_timeout(None) >= MIN_DECISION_TIMEOUT);
    }

    #[test]
    fn prompt_mode_parks_and_announces_with_the_request_fields() {
        let (b, out, sink) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        assert_eq!(
            b.handle_control_request(&request(WRITE_REQ)),
            ControlHandling::Parked
        );
        assert!(
            out.lines().is_empty(),
            "nothing is answered until the operator decides"
        );
        let pending = b.pending();
        assert_eq!(pending.len(), 1);
        let n = &pending[0];
        assert_eq!(n.request_id, "req-9");
        assert_eq!(n.tool_name, "Write");
        assert_eq!(
            n.input,
            serde_json::json!({"file_path":"/w/probe.txt","content":"hello"})
        );
        assert_eq!(n.suggestions.len(), 1, "mapped from permission_suggestions");
        assert_eq!(n.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(n.timeout_secs, 60);
        assert_eq!(
            sink.requested.lock().unwrap().as_slice(),
            pending.as_slice()
        );
        // camelCase on the wire.
        let wire = serde_json::to_value(n).unwrap();
        for key in [
            "sessionId",
            "requestId",
            "toolName",
            "input",
            "suggestions",
            "expiresAt",
        ] {
            assert!(wire.get(key).is_some(), "{key} in {wire}");
        }
    }

    #[test]
    fn operator_allow_echoes_the_original_input_and_clears_the_request() {
        let (b, out, sink) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        b.handle_control_request(&request(WRITE_REQ));
        let resolved = b
            .respond(
                "req-9",
                PermissionDecision::Allow {
                    updated_input: None,
                },
            )
            .unwrap();
        assert_eq!(resolved.outcome, PermissionOutcome::Allowed);
        assert_eq!(
            out.lines(),
            vec![
                r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-9","response":{"behavior":"allow","updatedInput":{"content":"hello","file_path":"/w/probe.txt"}}}}"#
            ]
        );
        assert!(b.pending().is_empty());
        assert_eq!(sink.resolved.lock().unwrap().len(), 1);
        // A second answer to the same request is refused, not re-sent.
        assert!(b
            .respond(
                "req-9",
                PermissionDecision::Allow {
                    updated_input: None
                }
            )
            .is_err());
        assert_eq!(out.lines().len(), 1);
    }

    #[test]
    fn operator_allow_with_an_edited_input_sends_the_edit() {
        let (b, out, _) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        b.handle_control_request(&request(WRITE_REQ));
        assert!(
            b.respond(
                "req-9",
                PermissionDecision::Allow {
                    updated_input: Some(serde_json::json!("x"))
                }
            )
            .is_err(),
            "a non-object input is refused"
        );
        assert_eq!(
            b.pending().len(),
            1,
            "a refused edit leaves the request parked"
        );
        b.respond(
            "req-9",
            PermissionDecision::Allow {
                updated_input: Some(serde_json::json!({"file_path":"/w/other.txt","content":"hi"})),
            },
        )
        .unwrap();
        assert!(out.lines()[0]
            .contains(r#""updatedInput":{"content":"hi","file_path":"/w/other.txt"}"#));
    }

    #[test]
    fn operator_deny_sends_the_message_and_reports_the_interrupt() {
        let (b, out, _) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        b.handle_control_request(&request(WRITE_REQ));
        let resolved = b
            .respond(
                "req-9",
                PermissionDecision::Deny {
                    message: None,
                    interrupt: true,
                },
            )
            .unwrap();
        assert_eq!(resolved.outcome, PermissionOutcome::Denied);
        assert!(resolved.interrupt);
        assert_eq!(
            out.lines(),
            vec![format!(
                r#"{{"type":"control_response","response":{{"subtype":"success","request_id":"req-9","response":{{"behavior":"deny","message":"{DEFAULT_DENY_MESSAGE}"}}}}}}"#
            )]
        );
    }

    #[test]
    fn an_unanswered_request_is_denied_at_the_bound() {
        let (b, out, sink) = broker(PermissionMode::Prompt, Duration::from_millis(50));
        b.handle_control_request(&request(WRITE_REQ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while out.lines().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let lines = out.lines();
        assert_eq!(lines.len(), 1, "denied exactly once");
        let sent: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(sent["response"]["subtype"], "success");
        assert_eq!(sent["response"]["request_id"], "req-9");
        assert_eq!(sent["response"]["response"]["behavior"], "deny");
        assert!(sent["response"]["response"]["message"]
            .as_str()
            .unwrap()
            .contains("No permission decision within"));
        assert!(b.pending().is_empty());
        let resolved = sink.resolved.lock().unwrap().clone();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].outcome, PermissionOutcome::TimedOut);
        // The operator's late answer finds nothing to answer.
        assert!(b
            .respond(
                "req-9",
                PermissionDecision::Allow {
                    updated_input: None
                }
            )
            .is_err());
        assert_eq!(out.lines().len(), 1);
    }

    #[test]
    fn an_answered_request_never_times_out() {
        let (b, out, sink) = broker(PermissionMode::Prompt, Duration::from_millis(80));
        b.handle_control_request(&request(WRITE_REQ));
        b.respond(
            "req-9",
            PermissionDecision::Allow {
                updated_input: None,
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(out.lines().len(), 1);
        assert_eq!(sink.resolved.lock().unwrap().len(), 1);
    }

    #[test]
    fn bypass_mode_allows_in_the_sdk_shape_without_parking() {
        for mode in [
            PermissionMode::BypassPermissions,
            PermissionMode::DangerouslySkip,
        ] {
            let (b, out, sink) = broker(mode, Duration::from_secs(60));
            assert_eq!(
                b.handle_control_request(&request(WRITE_REQ)),
                ControlHandling::Allowed
            );
            assert_eq!(
                out.lines(),
                vec![
                    r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-9","response":{"behavior":"allow","updatedInput":{"content":"hello","file_path":"/w/probe.txt"}}}}"#
                ]
            );
            assert!(b.pending().is_empty());
            assert!(sink.requested.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn unknown_subtypes_and_malformed_requests_get_an_error_never_an_allow() {
        for mode in [PermissionMode::BypassPermissions, PermissionMode::Prompt] {
            let (b, out, _) = broker(mode, Duration::from_secs(60));
            let handled = b.handle_control_request(&request(
                r#"{"type":"control_request","request_id":"r1","request":{"subtype":"hook_callback","callback_id":"x"}}"#,
            ));
            assert_eq!(
                handled,
                ControlHandling::Refused(
                    "unsupported control request subtype: hook_callback".to_string()
                )
            );
            let malformed = b.handle_control_request(&request(
                r#"{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","input":{}}}"#,
            ));
            assert!(
                matches!(malformed, ControlHandling::Refused(ref e) if e.contains("tool_name"))
            );
            let lines = out.lines();
            assert_eq!(
                lines[0],
                r#"{"type":"control_response","response":{"subtype":"error","request_id":"r1","error":"unsupported control request subtype: hook_callback"}}"#
            );
            assert!(lines[1].starts_with(r#"{"type":"control_response","response":{"subtype":"error","request_id":"r2","error":"malformed can_use_tool request"#));
            for line in &lines {
                assert!(!line.contains("allow"), "{line}");
            }
            assert!(b.pending().is_empty());
        }
    }

    #[test]
    fn a_request_without_an_id_is_not_answered() {
        let (b, out, _) = broker(PermissionMode::BypassPermissions, Duration::from_secs(60));
        let handled = b.handle_control_request(&request(
            r#"{"type":"control_request","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{}}}"#,
        ));
        assert_eq!(handled, ControlHandling::Unanswerable);
        assert!(out.lines().is_empty());
    }

    // ── Every spawn site's posture, pinned from the source ───────────────

    const BYPASS: &str = "crate::session::launch_spec::PermissionMode::BypassPermissions";
    /// Spelled in two halves so this test is not itself a spawn site to
    /// `runner_spawn_sites`' census.
    const SPAWN: &str = concat!("ClaudeSession", "::spawn(");

    /// `src` with every comment and the CONTENTS of every string / char
    /// literal blanked to spaces (delimiters kept), so a search or a brace
    /// count sees code only — a `"https://…"` or a `"{"` cannot fool it.
    fn code_only(src: &str) -> String {
        let chars: Vec<char> = src.chars().collect();
        let mut out = String::with_capacity(src.len());
        let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if c == '/' && next == Some('/') {
                while i < chars.len() && chars[i] != '\n' {
                    out.push(' ');
                    i += 1;
                }
            } else if c == '/' && next == Some('*') {
                let mut depth = 0usize;
                while i < chars.len() {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        out.push_str("  ");
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        out.push_str("  ");
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        out.push(blank(chars[i]));
                        i += 1;
                    }
                }
            } else if c == 'r' && (next == Some('#') || next == Some('"')) && {
                let mut k = i + 1;
                while chars.get(k) == Some(&'#') {
                    k += 1;
                }
                chars.get(k) == Some(&'"')
            } {
                // Raw string r#"…"#.
                let mut k = i + 1;
                let mut hashes = 0;
                while chars[k] == '#' {
                    hashes += 1;
                    k += 1;
                }
                for &ch in &chars[i..=k] {
                    out.push(ch);
                }
                i = k + 1;
                while i < chars.len() {
                    if chars[i] == '"' && (0..hashes).all(|h| chars.get(i + 1 + h) == Some(&'#')) {
                        out.push('"');
                        out.extend(std::iter::repeat_n('#', hashes));
                        i += 1 + hashes;
                        break;
                    }
                    out.push(blank(chars[i]));
                    i += 1;
                }
            } else if c == '"' {
                out.push('"');
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(' ');
                        i += 1;
                    }
                    if i < chars.len() {
                        out.push(blank(chars[i]));
                        i += 1;
                    }
                }
                out.push('"');
                i += 1;
            } else if c == '\'' && (next == Some('\\') || chars.get(i + 2) == Some(&'\'')) {
                // A char literal ('{', '\n'); a lifetime has no closing quote.
                out.push('\'');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' {
                        out.push(' ');
                        i += 1;
                    }
                    if i < chars.len() {
                        out.push(' ');
                        i += 1;
                    }
                }
                out.push('\'');
                i += 1;
            } else {
                out.push(c);
                i += 1;
            }
        }
        out
    }

    /// [`code_only`] with every `#[cfg(test)]`-gated `mod … { … }` removed
    /// by brace depth — production code below a mid-file test module stays.
    fn production_code(src: &str) -> String {
        let code = code_only(src);
        let mut out = String::with_capacity(code.len());
        let mut rest = code.as_str();
        while let Some(at) = rest.find("#[cfg(test)]") {
            let (before, after) = rest.split_at(at);
            out.push_str(before);
            let after = after.get("#[cfg(test)]".len()..).unwrap_or("");
            let trimmed = after.trim_start();
            let gated_mod = trimmed.starts_with("mod ")
                || trimmed.starts_with("pub mod ")
                || trimmed.starts_with("pub(crate) mod ");
            match (gated_mod, after.find('{'), after.find(';')) {
                (true, Some(open), semi) if semi.is_none_or(|s| s > open) => {
                    let mut depth = 0usize;
                    let mut end = after.len();
                    for (k, ch) in after.char_indices().skip_while(|(k, _)| *k < open) {
                        match ch {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = k + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    rest = after.get(end..).unwrap_or("");
                }
                // A gated item that is not an inline module: keep scanning.
                _ => rest = after,
            }
        }
        out.push_str(rest);
        out
    }

    /// The last argument of every `pattern` call in `file` (relative to
    /// `src/`), read from code only (comments and literals blanked).
    fn spawn_permission_args(file: &str, pattern: &str) -> Vec<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(file);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
        let code = code_only(&src);
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(at) = code.get(from..).and_then(|rest| rest.find(pattern)) {
            let start = from + at + pattern.len();
            let mut depth = 1usize;
            let mut end = start;
            for (i, c) in code.get(start..).unwrap_or("").char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            let args = code.get(start..end).unwrap_or("");
            // Split on top-level commas.
            let (mut parts, mut cur, mut d) = (Vec::new(), String::new(), 0i32);
            for c in args.chars() {
                match c {
                    '(' | '[' | '{' => d += 1,
                    ')' | ']' | '}' => d -= 1,
                    ',' if d == 0 => {
                        parts.push(std::mem::take(&mut cur));
                        continue;
                    }
                    _ => {}
                }
                cur.push(c);
            }
            parts.push(cur);
            let last = parts
                .into_iter()
                .map(|p| p.split_whitespace().collect::<String>())
                .rev()
                .find(|p| !p.is_empty())
                .unwrap_or_default();
            out.push(last);
            from = end;
        }
        out
    }

    #[test]
    fn the_source_scanner_sees_code_only() {
        let src = [
            "let a = \"",
            SPAWN,
            "\"; // ",
            SPAWN,
            "\nlet b = '{'; let c: &'static str = r#\"x { \" y\"#;\n",
            "#[cfg(test)]\nmod tests { fn t() { let _ = PermissionMode::Prompt; } }\n",
            "fn prod() { PermissionMode::Prompt }\n",
        ]
        .concat();
        let code = code_only(&src);
        assert!(!code.contains(SPAWN), "{code}");
        assert!(!code.contains("'{'"));
        let prod = production_code(&src);
        assert_eq!(prod.matches("PermissionMode::Prompt").count(), 1, "{prod}");
        assert!(prod.contains("fn prod()"));
    }

    /// Plan Phase 9: `Prompt` is never a default, and every existing
    /// autonomous spawn site keeps bypass — the seven the plan's Why names
    /// (`claude_session/runner.rs`, `commands/ai_session.rs` create + resume,
    /// `mcp/backend_relay.rs`, `mcp/sessions.rs`, `mcp/task_runs.rs`,
    /// `orchestration_loop/ai_session_executor.rs`, which is also the Conductor
    /// worker spawn). `create_ai_session` reaches its spawn through
    /// `open_ai_session(…, AiSessionLaunch::chat())`, which is bypass; the two
    /// in-place respawns carry the dead session's own posture.
    #[test]
    fn every_spawn_site_keeps_its_permission_posture() {
        let expect: &[(&str, &str, &[&str])] = &[
            ("claude_session/runner.rs", SPAWN, &[BYPASS]),
            ("commands/ai_session.rs", SPAWN, &["permission", BYPASS]),
            ("mcp/backend_relay.rs", SPAWN, &[BYPASS]),
            ("mcp/sessions.rs", SPAWN, &[BYPASS]),
            ("mcp/task_runs.rs", SPAWN, &[BYPASS]),
            (
                "orchestration_loop/ai_session_executor.rs",
                SPAWN,
                &[BYPASS],
            ),
            (
                "claude_session/session.rs",
                "Self::spawn(",
                &["self.permissions.mode()", "permission"],
            ),
        ];
        for (file, pattern, want) in expect {
            assert_eq!(&spawn_permission_args(file, pattern), want, "{file}");
        }
        // `create_ai_session` — the UI chat — stays bypass.
        let ai = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/ai_session.rs"),
        )
        .unwrap();
        assert!(
            ai.contains("AiSessionLaunch::chat(),"),
            "create_ai_session must open a chat"
        );
    }

    /// No production code outside the three files that implement the prompted
    /// launch may even name `PermissionMode::Prompt` — a new autonomous site
    /// cannot opt into prompting by accident.
    #[test]
    fn only_the_structured_launch_names_prompt_mode() {
        let allowed = [
            "session/launch_spec.rs",
            "claude_session/permission.rs",
            "commands/structured_session.rs",
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root.clone()];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    // Production code only (a test module may build a prompted
                    // broker to exercise it), and code only (a doc comment may
                    // name the variant).
                    if production_code(&text).contains("PermissionMode::Prompt")
                        && !allowed.contains(&rel.as_str())
                    {
                        offenders.push(rel);
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "PermissionMode::Prompt named outside the structured launch: {offenders:?}"
        );
    }

    #[test]
    fn session_end_resolves_every_parked_request_without_writing() {
        let (b, out, sink) = broker(PermissionMode::Prompt, Duration::from_secs(60));
        b.handle_control_request(&request(WRITE_REQ));
        b.end_session("the session closed");
        assert!(b.pending().is_empty());
        assert!(out.lines().is_empty(), "there is no CLI to answer");
        let resolved = sink.resolved.lock().unwrap().clone();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].outcome, PermissionOutcome::SessionEnded);
        b.end_session("again");
        assert_eq!(sink.resolved.lock().unwrap().len(), 1, "idempotent");
    }
}
