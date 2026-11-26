//! The operator's own remote end — the twin `operator_remote_session_end`
//! (plan `2026-10-28-one-session-lifecycle-authority`, Phase 16 (c), node
//! R0b-2b).
//!
//! `RemoteSessionEndFlow` (the operator's confirm dialog) alone calls it. It
//! sends the end through the web backend's USER-SESSION terminal path
//! (`/api/v1/devices/{device_id}/terminal`, qontinui-web
//! `runner_terminal_ws.py`) under the operator's Cognito access token, which
//! the relay stamps `origin: user_session` on the frame it forwards. The token
//! is the runner's one user-bearer door
//! (`device_jwt_refresher::ensure_fresh_cognito_bearer`), carried as the
//! `access_token` COOKIE on the upgrade — never `?token=`, which request and
//! access logs record. The twin is NOT on either UI-Bridge borrow route (not
//! in `UI_BRIDGE_COMMANDS`, not in `ALLOWED_PROXIED_COMMANDS`): the operator's
//! Cognito token is never handed to an agent.
//!
//! The decision (`operator_end_with`) is pure over injected transports, as
//! `end_remote_session_with` is, so every fallback is unit-testable:
//!
//! - no sign-in: a graceful end goes as today's device-socket end (a product
//!   close); a `force` is refused `operator_sign_in_required`;
//! - a target that predates Phase 93 answers a stampless end
//!   `remote_block_required`: graceful falls back to the device-socket end,
//!   `force` to the user-session `terminal_close` of the target's terminal id
//!   (which such a target executes as today's force);
//! - any other answer (`operator_stamp_required` included) is reported as it
//!   came, and nothing more is sent; no answer is `unknown`, never a fallback;
//! - a connect the relay rejects before its `connected` frame sent nothing, so
//!   it falls back: graceful to the device-socket end, `force` refused with the
//!   cause the CLOSE frame's typed code names (`runner_not_owned` ->
//!   `operator_not_device_owner`, `authentication_required` ->
//!   `operator_sign_in_required`, `authentication_failed` after one forced
//!   token refresh -> `operator_sign_in_required`, no typed code ->
//!   `operator_path_rejected`).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    handshake::client::Request as HttpRequest,
    http::{header, HeaderValue},
    Message,
};
use tracing::{info, warn};

use super::remote_end::{
    open_tab_grant_terminal, EndGrantSource, EndRequest, RemoteSessionEndResult,
};
use crate::mcp::remote_terminal::{parse_ended, EndOutcome, EndReply, END_TIMEOUT};
use crate::terminal::TerminalManager;

/// The relay refused the answer a force needs, because no terminal id is known.
pub(crate) const TARGET_TERMINAL_UNKNOWN: &str = "target_terminal_unknown";
pub(crate) const OPERATOR_SIGN_IN_REQUIRED: &str = "operator_sign_in_required";
pub(crate) const OPERATOR_NOT_DEVICE_OWNER: &str = "operator_not_device_owner";
pub(crate) const OPERATOR_PATH_REJECTED: &str = "operator_path_rejected";
/// What a target that predates Phase 93 answers an end with no relay `remote`
/// block.
pub(crate) const REMOTE_BLOCK_REQUIRED: &str = "remote_block_required";

/// Bound on the connect phase (TCP + TLS + upgrade + the relay's `connected`).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A typed connect-time rejection the web backend's user-session path names
/// (`runner_terminal_ws.py`, `CODE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RejectCode {
    AuthenticationRequired,
    AuthenticationFailed,
    RunnerNotOwned,
}

impl RejectCode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "authentication_required" => Some(Self::AuthenticationRequired),
            "authentication_failed" => Some(Self::AuthenticationFailed),
            "runner_not_owned" => Some(Self::RunnerNotOwned),
            _ => None,
        }
    }
}

/// The typed code of a rejection. The CLOSE frame's reason prefix
/// (`runner_not_owned: …`) is what a client reliably observes — the error
/// frame races the close and can be dropped — so it wins; the error frame's
/// `code` is the fallback. `None` is a rejection with no typed code.
pub(crate) fn rejection_code(
    error_frame_code: Option<&str>,
    close_reason: Option<&str>,
) -> Option<RejectCode> {
    let from_close = close_reason
        .and_then(|r| r.split(':').next())
        .and_then(RejectCode::parse);
    from_close.or_else(|| error_frame_code.and_then(RejectCode::parse))
}

/// What the user-session exchange is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserSessionAct {
    /// `terminal_end {session_id, force, request_id}`.
    End { session_id: String, force: bool },
    /// `terminal_close {terminal_id, request_id}` — a target that predates
    /// Phase 93 executes it as today's force.
    Close { terminal_id: String },
}

/// How one user-session exchange went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserSessionResult {
    /// The target's (or the relay's) answer, correlated by `request_id`.
    Answered(EndReply),
    /// The act was sent and no answer came: `unknown`, nothing more is sent.
    SentNoAnswer(String),
    /// The connect failed or was rejected before `connected`: nothing was sent.
    ConnectRejected {
        code: Option<RejectCode>,
        reason: String,
    },
}

/// The code a reply's reason leads with (`code: message`).
pub(crate) fn reply_code(reply: &EndReply) -> Option<&str> {
    reply
        .reason
        .as_deref()
        .and_then(|r| r.split(':').next())
        .map(str::trim)
        .filter(|c| !c.is_empty())
}

fn local_result(
    request: &EndRequest,
    session: uuid::Uuid,
    outcome: EndOutcome,
    reason: String,
    source: EndGrantSource,
) -> RemoteSessionEndResult {
    RemoteSessionEndResult {
        outcome,
        device_id: request.device_id.trim().to_string(),
        session_id: session.to_string(),
        terminal_id: request.terminal_id.clone(),
        via: None,
        reason: Some(reason),
        grant_source: source,
    }
}

fn from_reply(
    reply: EndReply,
    request: &EndRequest,
    session: uuid::Uuid,
) -> RemoteSessionEndResult {
    RemoteSessionEndResult {
        outcome: reply.outcome,
        device_id: request.device_id.trim().to_string(),
        session_id: reply.session_id.unwrap_or_else(|| session.to_string()),
        terminal_id: reply.terminal_id.or_else(|| request.terminal_id.clone()),
        via: reply.via,
        reason: reply.reason,
        grant_source: EndGrantSource::UserSession,
    }
}

/// `force` refused for a connect that never reached `connected`, with the cause
/// the close frame's typed code names.
fn forced_refusal(code: Option<RejectCode>, reason: &str) -> (&'static str, String) {
    match code {
        Some(RejectCode::RunnerNotOwned) => (
            OPERATOR_NOT_DEVICE_OWNER,
            format!(
                "{OPERATOR_NOT_DEVICE_OWNER}: the signed-in operator does not own that device, \
                 so the forced end was not sent"
            ),
        ),
        Some(RejectCode::AuthenticationRequired) | Some(RejectCode::AuthenticationFailed) => (
            OPERATOR_SIGN_IN_REQUIRED,
            format!(
                "{OPERATOR_SIGN_IN_REQUIRED}: the operator's sign-in was not accepted — sign in \
                 again to force; nothing was sent"
            ),
        ),
        None => (
            OPERATOR_PATH_REJECTED,
            format!(
                "{OPERATOR_PATH_REJECTED}: the web backend rejected the operator path ({reason}); \
                 nothing was sent"
            ),
        ),
    }
}

/// The whole decision, with the bearer, the user-session exchange and the
/// device-socket end injected.
///
/// - `token(force_refresh)` -> the operator's Cognito access token, `None`
///   without a sign-in; `true` asks for a FORCED refresh first;
/// - `user_session(token, act)` -> one exchange over the user-session path;
/// - `device_end()` -> today's device-socket graceful end.
pub(crate) async fn operator_end_with<T, TF, U, UF, D, DF>(
    request: &EndRequest,
    session: uuid::Uuid,
    terminal_hint: Option<String>,
    token: T,
    user_session: U,
    device_end: D,
) -> RemoteSessionEndResult
where
    T: Fn(bool) -> TF,
    TF: Future<Output = Option<String>>,
    U: Fn(String, UserSessionAct) -> UF,
    UF: Future<Output = UserSessionResult>,
    D: FnOnce() -> DF,
    DF: Future<Output = RemoteSessionEndResult>,
{
    let force = request.force;
    let Some(mut bearer) = token(false).await.filter(|t| !t.trim().is_empty()) else {
        // No operator sign-in: the graceful end is a product close; a force is
        // the operator's act and needs the sign-in.
        return if force {
            local_result(
                request,
                session,
                EndOutcome::Refused,
                format!(
                    "{OPERATOR_SIGN_IN_REQUIRED}: forcing a remote end needs the operator's \
                     sign-in; nothing was sent"
                ),
                EndGrantSource::None,
            )
        } else {
            device_end().await
        };
    };

    let end_act = UserSessionAct::End {
        session_id: session.to_string(),
        force,
    };
    let mut attempt = user_session(bearer.clone(), end_act.clone()).await;
    // `authentication_failed`: ONE forced token refresh and retry.
    if matches!(
        attempt,
        UserSessionResult::ConnectRejected {
            code: Some(RejectCode::AuthenticationFailed),
            ..
        }
    ) {
        match token(true).await.filter(|t| !t.trim().is_empty()) {
            Some(fresh) => {
                bearer = fresh;
                attempt = user_session(bearer.clone(), end_act).await;
            }
            None => {
                // The refresh yielded nothing: the same as no usable sign-in.
                attempt = UserSessionResult::ConnectRejected {
                    code: Some(RejectCode::AuthenticationRequired),
                    reason: "no operator token after a forced refresh".to_string(),
                };
            }
        }
    }

    match attempt {
        UserSessionResult::Answered(reply) => {
            if reply.outcome == EndOutcome::Refused
                && reply_code(&reply) == Some(REMOTE_BLOCK_REQUIRED)
            {
                // A target that predates Phase 93 refused a stampless end
                // before looking anything up. Fall back on exactly that.
                if !force {
                    info!(session = %session, "operator end: target predates the stamp — device-socket end");
                    return device_end().await;
                }
                let Some(terminal) = terminal_hint
                    .or_else(|| reply.terminal_id.clone())
                    .or_else(|| request.terminal_id.clone())
                    .filter(|t| !t.trim().is_empty())
                else {
                    return local_result(
                        request,
                        session,
                        EndOutcome::Refused,
                        format!(
                            "{TARGET_TERMINAL_UNKNOWN}: the target's terminal for that session is \
                             not known, so the forced close was not sent"
                        ),
                        EndGrantSource::UserSession,
                    );
                };
                return match user_session(
                    bearer,
                    UserSessionAct::Close {
                        terminal_id: terminal,
                    },
                )
                .await
                {
                    UserSessionResult::Answered(r) => from_reply(r, request, session),
                    UserSessionResult::SentNoAnswer(why) => local_result(
                        request,
                        session,
                        EndOutcome::Unknown,
                        why,
                        EndGrantSource::UserSession,
                    ),
                    UserSessionResult::ConnectRejected { reason, .. } => local_result(
                        request,
                        session,
                        EndOutcome::Unknown,
                        format!("user_session_close_connect_failed: {reason}"),
                        EndGrantSource::UserSession,
                    ),
                };
            }
            // Any other answer — `operator_stamp_required` included — as it came.
            from_reply(reply, request, session)
        }
        UserSessionResult::SentNoAnswer(why) => local_result(
            request,
            session,
            EndOutcome::Unknown,
            why,
            EndGrantSource::UserSession,
        ),
        UserSessionResult::ConnectRejected { code, reason } => {
            warn!(session = %session, code = ?code, %reason, "operator end: user-session connect rejected");
            if force {
                let (_code, message) = forced_refusal(code, &reason);
                local_result(
                    request,
                    session,
                    EndOutcome::Refused,
                    message,
                    EndGrantSource::None,
                )
            } else {
                device_end().await
            }
        }
    }
}

// ============================================================================
// The user-session transport
// ============================================================================

/// `{base}/api/v1/devices/{device_id}/terminal` — the token is NEVER in the URL.
pub(crate) fn user_session_ws_url(backend_base: &str, device_id: &str) -> String {
    format!(
        "{}/api/v1/devices/{}/terminal",
        backend_base
            .trim()
            .trim_end_matches('/')
            .replace("https://", "wss://")
            .replace("http://", "ws://"),
        urlencoding::encode(device_id.trim())
    )
}

/// The upgrade request: the operator's access token as the `access_token`
/// COOKIE (never `?token=`, which request and access logs record).
pub(crate) fn user_session_upgrade_request(
    url: &str,
    access_token: &str,
) -> Result<HttpRequest, String> {
    let mut req = url
        .into_client_request()
        .map_err(|e| format!("invalid ws URL: {e}"))?;
    let mut cookie = HeaderValue::from_str(&format!("access_token={}", access_token.trim()))
        .map_err(|e| format!("invalid cookie value: {e}"))?;
    cookie.set_sensitive(true);
    req.headers_mut().insert(header::COOKIE, cookie);
    Ok(req)
}

/// The frame one act sends.
pub(crate) fn act_frame(act: &UserSessionAct, request_id: &str) -> Value {
    match act {
        UserSessionAct::End { session_id, force } => json!({
            "type": "terminal_end",
            "session_id": session_id,
            "force": force,
            "request_id": request_id,
        }),
        UserSessionAct::Close { terminal_id } => json!({
            "type": "terminal_close",
            "terminal_id": terminal_id,
            "request_id": request_id,
        }),
    }
}

/// The answer a frame carries for `request_id`, if it is one.
pub(crate) fn answer_from_frame(
    frame: &Value,
    request_id: &str,
    act: &UserSessionAct,
) -> Option<EndReply> {
    if frame.get("request_id").and_then(|v| v.as_str()) != Some(request_id) {
        return None;
    }
    match frame.get("type").and_then(|v| v.as_str())? {
        "terminal_ended" => Some(parse_ended(frame)),
        "terminal_closed" if matches!(act, UserSessionAct::Close { .. }) => Some(EndReply {
            outcome: EndOutcome::Ended,
            session_id: None,
            terminal_id: frame
                .get("terminal_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            via: Some("force".to_string()),
            reason: None,
        }),
        "error" => {
            let text = |k: &str| {
                frame
                    .get(k)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty())
            };
            let reason = match (text("code"), text("message")) {
                (Some(c), Some(m)) => format!("{c}: {m}"),
                (Some(c), None) => c,
                (None, Some(m)) => m,
                (None, None) => "error".to_string(),
            };
            Some(EndReply::local(EndOutcome::Refused, reason))
        }
        _ => None,
    }
}

fn text_frame(msg: &Message) -> Option<Value> {
    match msg {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

fn close_reason(msg: &Message) -> Option<String> {
    match msg {
        Message::Close(Some(f)) => Some(f.reason.as_str().to_string()),
        _ => None,
    }
}

/// Drive one exchange over an open socket: wait for `connected` (a close
/// before it is a typed rejection), send the act, wait for its answer.
pub(crate) async fn drive_exchange<S>(
    mut ws: S,
    act: &UserSessionAct,
    request_id: &str,
    connect_timeout: Duration,
    answer_timeout: Duration,
) -> UserSessionResult
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    // Phase 1: until `connected`. Nothing has been sent, so any failure here is
    // a rejection the caller may fall back from.
    let mut error_code: Option<String> = None;
    let mut error_message: Option<String> = None;
    let connected = tokio::time::timeout(connect_timeout, async {
        loop {
            match ws.next().await {
                Some(Ok(msg)) => {
                    if let Some(reason) = close_reason(&msg) {
                        return Err(Some(reason));
                    }
                    if matches!(msg, Message::Close(None)) {
                        return Err(None);
                    }
                    if let Some(frame) = text_frame(&msg) {
                        match frame.get("type").and_then(|v| v.as_str()) {
                            Some("connected") => return Ok(()),
                            Some("error") => {
                                error_code = frame
                                    .get("code")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                error_message = frame
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                            }
                            _ => {}
                        }
                    }
                }
                Some(Err(e)) => return Err(Some(format!("transport: {e}"))),
                None => return Err(None),
            }
        }
    })
    .await;
    match connected {
        Ok(Ok(())) => {}
        Ok(Err(close)) => {
            let reason = close
                .clone()
                .or(error_message.clone())
                .unwrap_or_else(|| "closed before connected".to_string());
            return UserSessionResult::ConnectRejected {
                code: rejection_code(error_code.as_deref(), close.as_deref()),
                reason,
            };
        }
        Err(_elapsed) => {
            return UserSessionResult::ConnectRejected {
                code: rejection_code(error_code.as_deref(), None),
                reason: "connect timeout".to_string(),
            }
        }
    }

    // Phase 2: send, then wait for the correlated answer. From here a missing
    // answer is `unknown` and nothing more is sent.
    if let Err(e) = ws
        .send(Message::text(act_frame(act, request_id).to_string()))
        .await
    {
        return UserSessionResult::SentNoAnswer(format!(
            "send_failed: {e} — the end may or may not have been sent"
        ));
    }
    let waited = tokio::time::timeout(answer_timeout, async {
        while let Some(next) = ws.next().await {
            let Ok(msg) = next else { break };
            if matches!(msg, Message::Close(_)) {
                break;
            }
            if let Some(frame) = text_frame(&msg) {
                if let Some(reply) = answer_from_frame(&frame, request_id, act) {
                    return Some(reply);
                }
            }
        }
        None
    })
    .await;
    match waited {
        Ok(Some(reply)) => UserSessionResult::Answered(reply),
        Ok(None) => UserSessionResult::SentNoAnswer(
            "no_answer: the user-session socket closed before an answer — the session may or \
             may not have ended on the target"
                .to_string(),
        ),
        Err(_) => UserSessionResult::SentNoAnswer(format!(
            "timeout: no answer within {}s — the session may or may not have ended on the target",
            answer_timeout.as_secs()
        )),
    }
}

/// One real exchange: connect with the operator's access token as a cookie.
async fn user_session_exchange(
    backend_base: &str,
    device_id: &str,
    access_token: &str,
    act: &UserSessionAct,
) -> UserSessionResult {
    let url = user_session_ws_url(backend_base, device_id);
    let request = match user_session_upgrade_request(&url, access_token) {
        Ok(r) => r,
        Err(reason) => {
            return UserSessionResult::ConnectRejected { code: None, reason };
        }
    };
    let ws = match qontinui_runner_lib::outbound_net::connect_ws(request, CONNECT_TIMEOUT).await {
        Ok(ws) => ws,
        Err(e) => {
            return UserSessionResult::ConnectRejected {
                code: None,
                reason: format!("connect: {e}"),
            }
        }
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut ws = ws;
    let result = drive_exchange(&mut ws, act, &request_id, CONNECT_TIMEOUT, END_TIMEOUT).await;
    let _ = ws.close(None).await;
    result
}

/// The operator's Cognito access token through the runner's one user-bearer
/// door; `force_refresh` is the one retry after `authentication_failed`.
async fn operator_token(force_refresh: bool) -> Option<String> {
    let auth = crate::auth::AuthManager::new();
    let token = if force_refresh {
        crate::mcp::device_jwt_refresher::refresh_cognito_bearer(&auth, true)
            .await
            .0
    } else {
        crate::mcp::device_jwt_refresher::ensure_fresh_cognito_bearer(&auth).await
    };
    token.filter(|t| !t.trim().is_empty())
}

/// The `EndDoor::OperatorSingle` body (called by `end_remote_session`).
pub(crate) async fn end_as_operator(
    tm: &Arc<TerminalManager>,
    coord_base: &str,
    request: &EndRequest,
    session: uuid::Uuid,
) -> RemoteSessionEndResult {
    let backend = crate::api_config::get_api_base_url();
    let device = request.device_id.trim().to_string();
    // The graceful answer's terminal id (the flow passes it), else an open tab's.
    let terminal_hint = request
        .terminal_id
        .clone()
        .or_else(|| open_tab_grant_terminal(tm, &device, session));
    operator_end_with(
        request,
        session,
        terminal_hint,
        operator_token,
        |token, act| {
            let backend = backend.clone();
            let device = device.clone();
            async move { user_session_exchange(&backend, &device, &token, &act).await }
        },
        || async move {
            let graceful = EndRequest {
                force: false,
                ..request.clone()
            };
            super::remote_end::end_over_device_socket(tm, coord_base, &graceful, session).await
        },
    )
    .await
}

/// END a session on another device AS THE OPERATOR (plan
/// `2026-10-28-one-session-lifecycle-authority`, Phase 16 (c)). Called by
/// `RemoteSessionEndFlow` alone; absent from both UI-Bridge borrow routes.
#[tauri::command]
pub async fn operator_remote_session_end(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    device_id: String,
    session_id: String,
    force: Option<bool>,
    tenant: Option<String>,
    terminal_id: Option<String>,
) -> Result<RemoteSessionEndResult, String> {
    let base = super::remote_attach::coord_base_for(&app_handle);
    super::remote_end::end_remote_session(
        terminal_manager.inner(),
        &base,
        EndRequest {
            device_id,
            session_id,
            force: force.unwrap_or(false),
            tenant,
            terminal_id,
        },
        super::remote_end::EndDoor::OperatorSingle,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};

    fn req(force: bool) -> EndRequest {
        EndRequest {
            device_id: "dev-1".into(),
            session_id: "7b0e1a52-8a58-4a41-8d86-0a4e2d6c1f11".into(),
            force,
            tenant: None,
            terminal_id: None,
        }
    }

    fn session() -> uuid::Uuid {
        uuid::Uuid::parse_str("7b0e1a52-8a58-4a41-8d86-0a4e2d6c1f11").unwrap()
    }

    fn refused(reason: &str) -> EndReply {
        EndReply {
            outcome: EndOutcome::Refused,
            session_id: None,
            terminal_id: None,
            via: None,
            reason: Some(reason.to_string()),
        }
    }

    fn ended(via: &str) -> EndReply {
        EndReply {
            outcome: EndOutcome::Ended,
            session_id: None,
            terminal_id: Some("term-9".into()),
            via: Some(via.to_string()),
            reason: None,
        }
    }

    fn device_result() -> RemoteSessionEndResult {
        RemoteSessionEndResult {
            outcome: EndOutcome::Refused,
            device_id: "dev-1".into(),
            session_id: session().to_string(),
            terminal_id: None,
            via: None,
            reason: Some("device_socket".into()),
            grant_source: EndGrantSource::Minted,
        }
    }

    /// Records every act sent, answers from a script, and counts device ends
    /// and token reads.
    struct Rig {
        acts: Mutex<Vec<(String, UserSessionAct)>>,
        script: Mutex<VecDeque<UserSessionResult>>,
        device_ends: Mutex<u32>,
        token_calls: Mutex<Vec<bool>>,
        signed_in: bool,
    }

    impl Rig {
        fn new(signed_in: bool, script: Vec<UserSessionResult>) -> Self {
            Self {
                acts: Mutex::new(vec![]),
                script: Mutex::new(script.into()),
                device_ends: Mutex::new(0),
                token_calls: Mutex::new(vec![]),
                signed_in,
            }
        }

        async fn run(&self, force: bool, hint: Option<&str>) -> RemoteSessionEndResult {
            operator_end_with(
                &req(force),
                session(),
                hint.map(str::to_string),
                |refresh| async move {
                    self.token_calls.lock().unwrap().push(refresh);
                    self.signed_in
                        .then(|| if refresh { "fresh" } else { "tok" }.to_string())
                },
                |tok, act| async move {
                    self.acts.lock().unwrap().push((tok, act));
                    self.script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("an unscripted user-session exchange")
                },
                || async {
                    *self.device_ends.lock().unwrap() += 1;
                    device_result()
                },
            )
            .await
        }

        fn acts(&self) -> Vec<UserSessionAct> {
            self.acts
                .lock()
                .unwrap()
                .iter()
                .map(|a| a.1.clone())
                .collect()
        }
    }

    fn rejected(code: Option<RejectCode>) -> UserSessionResult {
        UserSessionResult::ConnectRejected {
            code,
            reason: "closed".into(),
        }
    }

    #[tokio::test]
    async fn a_stamped_answer_is_reported_as_it_came() {
        let rig = Rig::new(true, vec![UserSessionResult::Answered(ended("graceful"))]);
        let r = rig.run(false, None).await;
        assert_eq!(r.outcome, EndOutcome::Ended);
        assert_eq!(r.grant_source, EndGrantSource::UserSession);
        assert_eq!(*rig.device_ends.lock().unwrap(), 0);
        assert_eq!(rig.acts().len(), 1);
    }

    #[tokio::test]
    async fn without_a_sign_in_graceful_goes_to_the_device_socket() {
        let rig = Rig::new(false, vec![]);
        let r = rig.run(false, None).await;
        assert_eq!(r.reason.as_deref(), Some("device_socket"));
        assert_eq!(*rig.device_ends.lock().unwrap(), 1);
        assert!(rig.acts().is_empty());
    }

    #[tokio::test]
    async fn without_a_sign_in_force_is_refused_and_nothing_is_sent() {
        let rig = Rig::new(false, vec![]);
        let r = rig.run(true, Some("term-9")).await;
        assert_eq!(r.outcome, EndOutcome::Refused);
        assert!(r.reason.unwrap().starts_with("operator_sign_in_required"));
        assert_eq!(*rig.device_ends.lock().unwrap(), 0);
        assert!(rig.acts().is_empty());
    }

    #[tokio::test]
    async fn a_target_predating_the_stamp_gets_the_device_socket_end_when_graceful() {
        let rig = Rig::new(
            true,
            vec![UserSessionResult::Answered(refused(
                "remote_block_required: no remote block",
            ))],
        );
        let r = rig.run(false, None).await;
        assert_eq!(r.reason.as_deref(), Some("device_socket"));
        assert_eq!(*rig.device_ends.lock().unwrap(), 1);
        assert_eq!(rig.acts().len(), 1);
    }

    #[tokio::test]
    async fn a_target_predating_the_stamp_gets_a_user_session_close_when_forced() {
        let rig = Rig::new(
            true,
            vec![
                UserSessionResult::Answered(refused("remote_block_required: x")),
                UserSessionResult::Answered(EndReply {
                    outcome: EndOutcome::Ended,
                    session_id: None,
                    terminal_id: Some("term-9".into()),
                    via: Some("force".into()),
                    reason: None,
                }),
            ],
        );
        let r = rig.run(true, Some("term-9")).await;
        assert_eq!(r.outcome, EndOutcome::Ended);
        assert_eq!(r.via.as_deref(), Some("force"));
        assert_eq!(*rig.device_ends.lock().unwrap(), 0);
        let acts = rig.acts();
        assert_eq!(acts.len(), 2);
        assert_eq!(
            acts[1],
            UserSessionAct::Close {
                terminal_id: "term-9".into()
            }
        );
    }

    #[tokio::test]
    async fn a_forced_fallback_with_no_terminal_id_answers_target_terminal_unknown() {
        let rig = Rig::new(
            true,
            vec![UserSessionResult::Answered(refused(
                "remote_block_required: x",
            ))],
        );
        let r = rig.run(true, None).await;
        assert_eq!(r.outcome, EndOutcome::Refused);
        assert!(r.reason.unwrap().starts_with("target_terminal_unknown"));
        assert_eq!(rig.acts().len(), 1, "no close was sent");
    }

    #[tokio::test]
    async fn no_answer_or_any_other_code_means_no_second_act() {
        for force in [false, true] {
            let rig = Rig::new(
                true,
                vec![UserSessionResult::SentNoAnswer("timeout".into())],
            );
            let r = rig.run(force, Some("t")).await;
            assert_eq!(r.outcome, EndOutcome::Unknown);
            assert_eq!(rig.acts().len(), 1);
            assert_eq!(*rig.device_ends.lock().unwrap(), 0);

            for code in [
                "operator_stamp_required",
                "no_close_authority",
                "target_mismatch",
            ] {
                let rig = Rig::new(
                    true,
                    vec![UserSessionResult::Answered(refused(&format!(
                        "{code}: nope"
                    )))],
                );
                let r = rig.run(force, Some("t")).await;
                assert_eq!(r.outcome, EndOutcome::Refused);
                assert!(r.reason.unwrap().starts_with(code));
                assert_eq!(rig.acts().len(), 1, "{code}: no second act");
                assert_eq!(*rig.device_ends.lock().unwrap(), 0, "{code}");
            }
        }
    }

    #[tokio::test]
    async fn runner_not_owned_graceful_falls_back_and_force_is_refused_not_owner() {
        let rig = Rig::new(true, vec![rejected(Some(RejectCode::RunnerNotOwned))]);
        let r = rig.run(false, None).await;
        assert_eq!(r.reason.as_deref(), Some("device_socket"));

        let rig = Rig::new(true, vec![rejected(Some(RejectCode::RunnerNotOwned))]);
        let r = rig.run(true, Some("t")).await;
        assert_eq!(r.outcome, EndOutcome::Refused);
        assert!(r.reason.unwrap().starts_with("operator_not_device_owner"));
        assert_eq!(rig.acts().len(), 1, "nothing was sent after the rejection");
        assert_eq!(*rig.device_ends.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn authentication_required_forces_sign_in_required() {
        let rig = Rig::new(
            true,
            vec![rejected(Some(RejectCode::AuthenticationRequired))],
        );
        let r = rig.run(true, Some("t")).await;
        assert!(r.reason.unwrap().starts_with("operator_sign_in_required"));
    }

    #[tokio::test]
    async fn authentication_failed_refreshes_once_then_retries() {
        // Retry succeeds.
        let rig = Rig::new(
            true,
            vec![
                rejected(Some(RejectCode::AuthenticationFailed)),
                UserSessionResult::Answered(ended("graceful")),
            ],
        );
        let r = rig.run(true, Some("t")).await;
        assert_eq!(r.outcome, EndOutcome::Ended);
        assert_eq!(*rig.token_calls.lock().unwrap(), vec![false, true]);
        let toks: Vec<String> = rig
            .acts
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.0.clone())
            .collect();
        assert_eq!(toks, vec!["tok", "fresh"]);

        // Failed again after the refresh: sign-in required, exactly one retry.
        let rig = Rig::new(
            true,
            vec![
                rejected(Some(RejectCode::AuthenticationFailed)),
                rejected(Some(RejectCode::AuthenticationFailed)),
            ],
        );
        let r = rig.run(true, Some("t")).await;
        assert!(r.reason.unwrap().starts_with("operator_sign_in_required"));
        assert_eq!(rig.acts().len(), 2);
        assert_eq!(*rig.token_calls.lock().unwrap(), vec![false, true]);

        // Graceful after a second failure: the device-socket end.
        let rig = Rig::new(
            true,
            vec![
                rejected(Some(RejectCode::AuthenticationFailed)),
                rejected(Some(RejectCode::AuthenticationFailed)),
            ],
        );
        let r = rig.run(false, None).await;
        assert_eq!(r.reason.as_deref(), Some("device_socket"));
    }

    #[tokio::test]
    async fn a_close_with_no_typed_code_forces_path_rejected() {
        let rig = Rig::new(
            true,
            vec![UserSessionResult::ConnectRejected {
                code: None,
                reason: "policy violation".into(),
            }],
        );
        let r = rig.run(true, Some("t")).await;
        let reason = r.reason.unwrap();
        assert!(reason.starts_with("operator_path_rejected"), "{reason}");
        assert!(
            reason.contains("policy violation"),
            "the close reason is quoted"
        );
    }

    // ---- the wire ------------------------------------------------------

    #[test]
    fn the_upgrade_carries_the_token_as_a_cookie_and_never_in_the_url() {
        let url = user_session_ws_url("https://api.qontinui.io/", "dev-1");
        assert_eq!(url, "wss://api.qontinui.io/api/v1/devices/dev-1/terminal");
        assert!(!url.contains("token"));
        let req = user_session_upgrade_request(&url, "SECRET").unwrap();
        assert_eq!(req.uri().to_string(), url);
        assert_eq!(
            req.headers().get(header::COOKIE).unwrap(),
            "access_token=SECRET"
        );
        assert!(!req.uri().to_string().contains("SECRET"));
    }

    #[test]
    fn the_close_frames_code_wins_and_an_error_frame_code_is_the_fallback() {
        assert_eq!(
            rejection_code(None, Some("runner_not_owned: Runner not found")),
            Some(RejectCode::RunnerNotOwned)
        );
        assert_eq!(
            rejection_code(Some("authentication_required"), None),
            Some(RejectCode::AuthenticationRequired)
        );
        assert_eq!(
            rejection_code(Some("authentication_required"), Some("runner_not_owned: x")),
            Some(RejectCode::RunnerNotOwned)
        );
        assert_eq!(rejection_code(None, Some("Authentication failed")), None);
        assert_eq!(rejection_code(None, None), None);
    }

    #[test]
    fn the_frames_are_the_documented_shapes() {
        let f = act_frame(
            &UserSessionAct::End {
                session_id: "s".into(),
                force: true,
            },
            "r1",
        );
        assert_eq!(
            f,
            json!({"type":"terminal_end","session_id":"s","force":true,"request_id":"r1"})
        );
        let f = act_frame(
            &UserSessionAct::Close {
                terminal_id: "t".into(),
            },
            "r2",
        );
        assert_eq!(f["type"], "terminal_close");
        assert_eq!(f["terminal_id"], "t");
    }

    /// An in-memory socket: scripted inbound, recorded outbound.
    struct FakeWs {
        inbound: VecDeque<Result<Message, tokio_tungstenite::tungstenite::Error>>,
        sent: Arc<Mutex<Vec<Message>>>,
    }

    impl Stream for FakeWs {
        type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;
        fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.inbound.pop_front())
        }
    }

    impl Sink<Message> for FakeWs {
        type Error = tokio_tungstenite::tungstenite::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.sent.lock().unwrap().push(item);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn text(v: Value) -> Result<Message, tokio_tungstenite::tungstenite::Error> {
        Ok(Message::text(v.to_string()))
    }

    fn close(reason: &str) -> Result<Message, tokio_tungstenite::tungstenite::Error> {
        Ok(Message::Close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: reason.to_string().into(),
        })))
    }

    fn fake(
        inbound: Vec<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) -> (FakeWs, Arc<Mutex<Vec<Message>>>) {
        let sent = Arc::new(Mutex::new(vec![]));
        (
            FakeWs {
                inbound: inbound.into(),
                sent: sent.clone(),
            },
            sent,
        )
    }

    const T: Duration = Duration::from_secs(2);

    fn end_act() -> UserSessionAct {
        UserSessionAct::End {
            session_id: session().to_string(),
            force: false,
        }
    }

    /// A stub relay rejecting the connect `runner_not_owned`: the typed code is
    /// read off the close frame, whether or not the error frame was dropped,
    /// and nothing is sent.
    #[tokio::test]
    async fn a_rejected_connect_yields_its_typed_code_from_either_frame() {
        let with_error = vec![
            text(json!({"type":"error","message":"Runner not found","code":"runner_not_owned"})),
            close("runner_not_owned: Runner not found or not owned by you"),
        ];
        let dropped_error = vec![close(
            "runner_not_owned: Runner not found or not owned by you",
        )];
        for inbound in [with_error, dropped_error] {
            let (ws, sent) = fake(inbound);
            let r = drive_exchange(ws, &end_act(), "rid", T, T).await;
            match r {
                UserSessionResult::ConnectRejected { code, .. } => {
                    assert_eq!(code, Some(RejectCode::RunnerNotOwned))
                }
                other => panic!("{other:?}"),
            }
            assert!(sent.lock().unwrap().is_empty(), "nothing was sent");
        }
        // Error frame only, then the stream ends: the frame's code still reads.
        let (ws, _) = fake(vec![text(
            json!({"type":"error","message":"x","code":"authentication_required"}),
        )]);
        match drive_exchange(ws, &end_act(), "rid", T, T).await {
            UserSessionResult::ConnectRejected { code, .. } => {
                assert_eq!(code, Some(RejectCode::AuthenticationRequired))
            }
            other => panic!("{other:?}"),
        }
        // A close with no typed code.
        let (ws, _) = fake(vec![close("Authentication failed")]);
        match drive_exchange(ws, &end_act(), "rid", T, T).await {
            UserSessionResult::ConnectRejected { code, reason } => {
                assert_eq!(code, None);
                assert_eq!(reason, "Authentication failed");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_connected_socket_sends_the_end_and_reads_the_correlated_answer() {
        let (ws, sent) = fake(vec![
            text(json!({"type":"connected","runner_connected":true})),
            // Another request's answer is not ours.
            text(json!({"type":"terminal_ended","request_id":"other","outcome":"ended"})),
            text(
                json!({"type":"terminal_ended","request_id":"rid","outcome":"ended",
                "via":"graceful","terminal_id":"term-9"}),
            ),
        ]);
        let r = drive_exchange(ws, &end_act(), "rid", T, T).await;
        match r {
            UserSessionResult::Answered(reply) => {
                assert_eq!(reply.outcome, EndOutcome::Ended);
                assert_eq!(reply.via.as_deref(), Some("graceful"));
            }
            other => panic!("{other:?}"),
        }
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let frame: Value = serde_json::from_str(sent[0].to_text().unwrap()).unwrap();
        assert_eq!(frame["type"], "terminal_end");
        assert_eq!(frame["request_id"], "rid");
        assert_eq!(frame["force"], false);
    }

    #[tokio::test]
    async fn a_targets_refusal_error_frame_reaches_the_asker_with_its_code() {
        let (ws, _) = fake(vec![
            text(json!({"type":"connected"})),
            text(
                json!({"type":"error","request_id":"rid","code":"remote_block_required",
                "message":"no remote block"}),
            ),
        ]);
        match drive_exchange(ws, &end_act(), "rid", T, T).await {
            UserSessionResult::Answered(reply) => {
                assert_eq!(reply.outcome, EndOutcome::Refused);
                assert_eq!(reply_code(&reply), Some(REMOTE_BLOCK_REQUIRED));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn once_sent_a_missing_answer_is_unknown() {
        let (ws, _) = fake(vec![text(json!({"type":"connected"}))]);
        match drive_exchange(ws, &end_act(), "rid", T, T).await {
            UserSessionResult::SentNoAnswer(_) => {}
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_terminal_closed_answers_a_close_act_as_a_force() {
        let act = UserSessionAct::Close {
            terminal_id: "term-9".into(),
        };
        let (ws, sent) = fake(vec![
            text(json!({"type":"connected"})),
            text(json!({"type":"terminal_closed","request_id":"rid","terminal_id":"term-9"})),
        ]);
        match drive_exchange(ws, &act, "rid", T, T).await {
            UserSessionResult::Answered(reply) => {
                assert_eq!(reply.outcome, EndOutcome::Ended);
                assert_eq!(reply.via.as_deref(), Some("force"));
            }
            other => panic!("{other:?}"),
        }
        let frame: Value =
            serde_json::from_str(sent.lock().unwrap()[0].to_text().unwrap()).unwrap();
        assert_eq!(frame["type"], "terminal_close");
        assert_eq!(frame["terminal_id"], "term-9");
    }

    // ---- the source scan ---------------------------------------------------

    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// Nothing but the twin constructs `EndDoor::OperatorSingle` or takes the
    /// user-session path. `remote_end.rs` defines the enum and routes on it
    /// (`end_remote_session`); every other file naming either is a violation.
    #[test]
    fn only_the_twin_takes_the_user_session_path() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![];
        rs_files(&root, &mut files);
        assert!(files.len() > 100);
        let door_allowed = ["commands/operator_remote_end.rs", "commands/remote_end.rs"];
        let path_allowed = ["commands/operator_remote_end.rs"];
        let door_tokens = ["EndDoor::OperatorSingle"];
        let path_tokens = [
            "user_session_exchange",
            "user_session_upgrade_request",
            "user_session_ws_url",
            "operator_end_with",
            "end_as_operator",
        ];
        let mut violations = vec![];
        let mut twin_hits = 0;
        for f in &files {
            let rel = f
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(f).unwrap_or_default();
            for (n, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                let code = line.split("//").next().unwrap_or(line);
                for tok in door_tokens {
                    if code.contains(tok) {
                        if rel == "commands/operator_remote_end.rs" {
                            twin_hits += 1;
                        }
                        if !door_allowed.contains(&rel.as_str()) {
                            violations.push(format!("{rel}:{} names `{tok}`", n + 1));
                        }
                    }
                }
                for tok in path_tokens {
                    if code.contains(tok) {
                        if rel == "commands/operator_remote_end.rs" {
                            twin_hits += 1;
                        }
                        if !path_allowed.contains(&rel.as_str()) && rel != "commands/remote_end.rs"
                        {
                            violations.push(format!("{rel}:{} names `{tok}`", n + 1));
                        }
                    }
                }
            }
        }
        assert!(twin_hits > 0, "the scan found nothing in the twin: vacuous");
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }
}
