//! A real pane with a coord message waiting for it, on any runner (manual-test
//! loop iteration 2 of plan
//! `2026-11-27-coord-message-to-cross-account-pane-never-pushed`).
//!
//! ## Why this exists
//!
//! The session-message poller's terminal arm and the `PendingMessagesBadge`
//! in each zone header could only be watched on a runner that hosts a live
//! Claude pane AND has a coord message addressed to it. A temp runner spawned
//! from a worktree has neither: no terminals, so nothing to push into, and no
//! pending message, so nothing to badge. These two routes supply both through
//! the REAL paths:
//!
//! - `POST /ui-bridge/test/open-bound-terminal`
//!   `{"session_id":"<id>","config_dir":"<dir>","title":"<optional>"}` opens a
//!   real PTY through `TerminalManager::create` running a bare shell that
//!   prints one line (never `claude`), and writes an `open`, confirmed,
//!   `authoritative` lifecycle record binding `session_id` and `config_dir` to
//!   it, into the RUNNING store. The poller resolves a message to that pane by
//!   the record exactly as it resolves a real one.
//! - `POST /ui-bridge/test/seed-pending-message`
//!   `{"to_session":"<id>","message_id":"<optional>","priority":"<optional>",
//!   "body":"<optional>"}` adds a message the poller reads on its next tick as
//!   if coord's `GET /coord/session-messages/pending` had returned it. On an
//!   unpaired runner the tick then reads ONLY these messages (no coord pull);
//!   on a paired one they are appended to coord's set.
//!
//! The poller's own gate decides what happens next. A bare shell is not a live
//! `claude`, so the idle gate refuses the paste (`pty_never_idle` or
//! `target_not_live`), the message stays pending, and the tick's pending view
//! (`GET /session-messages/pending-view`, the `session-messages-pending-view`
//! event) lists it under the pane's terminal id — which is what the badge
//! renders as `terminal-pending-messages-<zone>`. Nothing is ever typed into
//! the shell: the seam reaches the gate, and the gate is what keeps a message
//! out of a pane with no claude in it.
//!
//! An injected message never reaches coord: the blocked-delivery POST and the
//! mark-delivered POST are skipped for it, and a delivery (possible only into a
//! pane that does run claude) drops it from this registry instead.
//!
//! ## Teardown — the module's TTL + clear-injected contract
//!
//! `POST /ui-bridge/test/clear-injected` closes every pane this seam opened,
//! closes its lifecycle record and drops every injected message. A pane or a
//! message older than [`super::SESSION_TTL`] is torn down on the poller's next
//! tick, so an abandoned run cannot leave either behind.
//!
//! While a seam pane is open its session id is held in the session in-flight
//! registry, so the lifecycle liveness poll does not close the record as
//! `poll-dead` for having no claude in its subtree (and no mover takes the
//! fake id meanwhile).

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use tauri::Manager as _;
use tracing::{info, warn};

use super::super::types::ApiState;
use super::{
    app_handle_slot, emit_injected_changed, record_from_seed, InjectSessionError,
    SeedLifecycleRecord, SESSION_TTL,
};
use crate::session::in_flight::SessionInFlight;
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, CLOSE_REASON_PTY_EXIT, ORIGIN_AUTHORITATIVE,
};

/// The in-flight holder label for a seam pane's session id.
const IN_FLIGHT_HOLDER: &str = "test-seam";

/// The line the seam pane's shell prints, so a human looking at it knows why
/// it is there.
const PANE_BANNER: &str = "qontinui test seam pane - no claude runs here";

// =============================================================================
// Injected pending messages
// =============================================================================

/// One message the poller reads as pending from coord.
#[derive(Debug, Clone)]
pub struct InjectedPendingMessage {
    pub message_id: String,
    pub to_session: String,
    pub priority: String,
    pub body: String,
    /// RFC 3339, rendered into the frame like coord's `created_at`.
    pub created_at: String,
    injected_at: Instant,
}

fn messages() -> &'static Mutex<Vec<InjectedPendingMessage>> {
    static MESSAGES: OnceLock<Mutex<Vec<InjectedPendingMessage>>> = OnceLock::new();
    MESSAGES.get_or_init(Default::default)
}

/// A pane this seam opened.
struct SeamTerminal {
    terminal_id: String,
    session_id: String,
    opened_at: Instant,
    /// Held for the pane's life; dropping it releases the session id.
    _hold: SessionInFlight,
}

fn terminals() -> &'static Mutex<Vec<SeamTerminal>> {
    static TERMINALS: OnceLock<Mutex<Vec<SeamTerminal>>> = OnceLock::new();
    TERMINALS.get_or_init(Default::default)
}

/// The poller's read, once per tick: tear down every pane and drop every
/// message past the TTL, then return the messages still injected.
pub fn poller_messages() -> Vec<InjectedPendingMessage> {
    let now = Instant::now();
    let expired: Vec<SeamTerminal> = {
        let mut guard = terminals().lock().unwrap_or_else(PoisonError::into_inner);
        let (expired, kept) = std::mem::take(&mut *guard)
            .into_iter()
            .partition(|t| now.duration_since(t.opened_at) >= SESSION_TTL);
        *guard = kept;
        expired
    };
    if !expired.is_empty() {
        info!(
            "test_fixtures: tearing down {} seam pane(s) past the TTL",
            expired.len()
        );
        close_all(expired);
    }
    let mut guard = messages().lock().unwrap_or_else(PoisonError::into_inner);
    evict_expired_messages(&mut guard, now);
    guard.clone()
}

fn evict_expired_messages(registry: &mut Vec<InjectedPendingMessage>, now: Instant) {
    registry.retain(|m| now.duration_since(m.injected_at) < SESSION_TTL);
}

/// The poller delivered `message_id`: it is no longer pending.
pub fn mark_delivered(message_id: &str) {
    messages()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|m| m.message_id != message_id);
}

/// `clear-injected`'s share: close every seam pane, drop every message.
/// Returns `(panes closed, messages dropped)`.
pub(super) fn clear_all() -> (usize, usize) {
    let panes = std::mem::take(&mut *terminals().lock().unwrap_or_else(PoisonError::into_inner));
    let pane_count = panes.len();
    close_all(panes);
    let mut guard = messages().lock().unwrap_or_else(PoisonError::into_inner);
    let message_count = guard.len();
    guard.clear();
    (pane_count, message_count)
}

fn close_all(panes: Vec<SeamTerminal>) {
    let Some(app) = app_handle_slot().get() else {
        return; // unit tests: no runner, nothing was opened
    };
    for pane in panes {
        close_seam_terminal(app, &pane);
    }
}

/// Close one seam pane and its record. The pane runs a bare shell this seam
/// started; no claude ever ran in it.
fn close_seam_terminal(app: &tauri::AppHandle, pane: &SeamTerminal) {
    if let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() {
        if let Err(e) = tm.close(&pane.terminal_id) {
            warn!(
                "test_fixtures: closing seam pane {} failed: {e}",
                pane.terminal_id
            );
        }
    }
    if let Some(store) = app.try_state::<Arc<SessionLifecycleStore>>() {
        store.record_close(&pane.session_id, CLOSE_REASON_PTY_EXIT);
    }
}

// =============================================================================
// POST /ui-bridge/test/open-bound-terminal
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct OpenBoundTerminalRequest {
    pub session_id: String,
    pub config_dir: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenBoundTerminalResponse {
    pub success: bool,
    pub terminal_id: String,
    pub session_id: String,
    pub config_dir: String,
}

type SeamError = (StatusCode, Json<InjectSessionError>);

fn seam_error(code: StatusCode, error: impl Into<String>) -> SeamError {
    (
        code,
        Json(InjectSessionError {
            success: false,
            error: error.into(),
        }),
    )
}

/// The seam pane's argv: a shell that prints [`PANE_BANNER`] and stays open.
fn pane_command() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/k".to_string(),
            format!("echo {PANE_BANNER}"),
        ]
    } else {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("echo '{PANE_BANNER}'; exec sh"),
        ]
    }
}

/// The lifecycle record binding `req` to `terminal_id`: open, confirmed now,
/// `authoritative` — the binding the poller routes a message on.
fn bound_record(
    req: &OpenBoundTerminalRequest,
    terminal_id: &str,
    now_ms: i64,
) -> Result<crate::session::session_lifecycle_store::TerminalSessionRecord, String> {
    record_from_seed(
        &SeedLifecycleRecord {
            session_id: req.session_id.trim().to_string(),
            state: "open".to_string(),
            last_seen_offset_ms: 0,
            closed_at_offset_ms: None,
            close_reason: None,
            page_id: None,
            zone_index: None,
            title: req.title.clone(),
            working_dir: None,
            confirmed_at: Some(0),
            restore_pending_at: None,
            restore_tier: None,
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            terminal_id: Some(terminal_id.to_string()),
            config_dir: Some(req.config_dir.trim().to_string()),
            write_transcript: false,
        },
        now_ms,
    )
}

pub(super) async fn open_bound_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<OpenBoundTerminalRequest>,
) -> Result<Json<OpenBoundTerminalResponse>, SeamError> {
    let session_id = req.session_id.trim().to_string();
    let config_dir = req.config_dir.trim().to_string();
    if session_id.is_empty() || config_dir.is_empty() {
        return Err(seam_error(
            StatusCode::BAD_REQUEST,
            "session_id and config_dir must be non-empty",
        ));
    }
    let app = state.app_handle.clone();
    let (Some(terminal_manager), Some(store)) = (
        app.try_state::<Arc<crate::terminal::TerminalManager>>()
            .map(|t| t.inner().clone()),
        app.try_state::<Arc<SessionLifecycleStore>>()
            .map(|s| s.inner().clone()),
    ) else {
        return Err(seam_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the terminal manager or the lifecycle store is not up yet",
        ));
    };
    // Never bind over a session this runner already hosts.
    if store.get(&session_id).is_some_and(|r| r.state == "open") {
        return Err(seam_error(
            StatusCode::CONFLICT,
            format!("session {session_id} already has an open lifecycle record"),
        ));
    }
    let hold = SessionInFlight::claim(&session_id, IN_FLIGHT_HOLDER).map_err(|holder| {
        seam_error(
            StatusCode::CONFLICT,
            format!("session {session_id} is in flight with {holder}"),
        )
    })?;

    let title = req.title.clone().or_else(|| Some("test seam".to_string()));
    let created = crate::spawn_off_worker::run_terminal_create_off_worker(move || {
        terminal_manager.create(
            title,
            None,
            None,
            None,
            None,
            app,
            Some(pane_command()),
            None,
            // Unattended: a seam call must respect the resource floor.
            false,
            // Nothing Claude-shaped is ever typed into this pane.
            crate::terminal::TrustArm::AccountChosenLater,
            None,
            crate::coord_drain_state::SpawnOrigin::Unknown,
        )
    })
    .await
    .map_err(|e| seam_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let pane = SeamTerminal {
        terminal_id: created.id.clone(),
        session_id: session_id.clone(),
        opened_at: Instant::now(),
        _hold: hold,
    };
    let record = match bound_record(
        &req,
        &pane.terminal_id,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(r) => r,
        Err(e) => {
            close_seam_terminal(&state.app_handle, &pane);
            return Err(seam_error(StatusCode::BAD_REQUEST, e));
        }
    };
    store.record_open(record);
    terminals()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(pane);
    info!(
        "test_fixtures: opened seam pane {} bound to session {session_id} under {config_dir}",
        created.id
    );
    emit_injected_changed("open-bound-terminal", 1);
    Ok(Json(OpenBoundTerminalResponse {
        success: true,
        terminal_id: created.id,
        session_id,
        config_dir,
    }))
}

// =============================================================================
// POST /ui-bridge/test/seed-pending-message
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct SeedPendingMessageRequest {
    pub to_session: String,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeedPendingMessageResponse {
    pub success: bool,
    pub message_id: String,
    pub to_session: String,
}

/// Validate `req` and add it to `registry`, replacing a message of the same
/// id. Returns `(message id, to_session)`.
fn insert_message_into(
    registry: &mut Vec<InjectedPendingMessage>,
    req: SeedPendingMessageRequest,
) -> Result<(String, String), String> {
    let to_session = req.to_session.trim().to_string();
    if to_session.is_empty() {
        return Err("to_session must be non-empty".to_string());
    }
    let message_id = match req.message_id.map(|s| s.trim().to_string()) {
        Some(id) if id.is_empty() => {
            return Err("message_id must be non-empty when present".to_string())
        }
        Some(id) => id,
        None => format!("test-seam-{}", uuid::Uuid::new_v4()),
    };
    let msg = InjectedPendingMessage {
        message_id: message_id.clone(),
        to_session: to_session.clone(),
        priority: req.priority.unwrap_or_default(),
        body: req
            .body
            .unwrap_or_else(|| "test seam message - safe to ignore".to_string()),
        created_at: chrono::Utc::now().to_rfc3339(),
        injected_at: Instant::now(),
    };
    registry.retain(|m| m.message_id != message_id);
    registry.push(msg);
    Ok((message_id, to_session))
}

pub(super) async fn seed_pending_message_handler(
    Json(req): Json<SeedPendingMessageRequest>,
) -> Result<Json<SeedPendingMessageResponse>, SeamError> {
    let (message_id, to_session) = insert_message_into(
        &mut messages().lock().unwrap_or_else(PoisonError::into_inner),
        req,
    )
    .map_err(|e| seam_error(StatusCode::BAD_REQUEST, e))?;
    info!("test_fixtures: seeded pending message {message_id} for session {to_session}");
    emit_injected_changed("seed-pending-message", 1);
    Ok(Json(SeedPendingMessageResponse {
        success: true,
        message_id,
        to_session,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(to: &str, id: Option<&str>) -> SeedPendingMessageRequest {
        SeedPendingMessageRequest {
            to_session: to.to_string(),
            message_id: id.map(str::to_string),
            priority: None,
            body: None,
        }
    }

    /// Seeding validates, generates an id when none is given, replaces a
    /// message of the same id, and the TTL evicts an old one. On a local
    /// registry: the global one is shared with every `clear-injected` test.
    #[test]
    fn seeding_validates_replaces_by_id_and_expires() {
        let mut reg = Vec::new();
        let (id, to) = insert_message_into(&mut reg, req("seam-sid", Some("seam-msg-1"))).unwrap();
        assert_eq!((id.as_str(), to.as_str()), ("seam-msg-1", "seam-sid"));
        insert_message_into(&mut reg, req("seam-sid", Some("seam-msg-1"))).unwrap();
        assert_eq!(reg.len(), 1, "a re-seed of one id replaces it");

        let (generated, _) = insert_message_into(&mut reg, req("seam-sid", None)).unwrap();
        assert!(generated.starts_with("test-seam-"));
        assert_eq!(reg.len(), 2);

        assert!(insert_message_into(&mut reg, req("  ", None)).is_err());
        assert!(insert_message_into(&mut reg, req("seam-sid", Some(" "))).is_err());
        assert_eq!(reg.len(), 2);

        evict_expired_messages(&mut reg, Instant::now());
        assert_eq!(reg.len(), 2, "fresh messages stay");
        evict_expired_messages(&mut reg, Instant::now() + SESSION_TTL);
        assert!(reg.is_empty(), "a message past the TTL is dropped");
    }

    /// The record the poller routes on: open, confirmed, authoritative, bound
    /// to the pane and the given dir.
    #[test]
    fn the_bound_record_is_one_the_poller_routes_on() {
        let req = OpenBoundTerminalRequest {
            session_id: " seam-bound ".to_string(),
            config_dir: "C:/claude/.claude-ben".to_string(),
            title: None,
        };
        let rec = bound_record(&req, "term-seam", 1_000_000).unwrap();
        assert_eq!(rec.claude_session_id, "seam-bound");
        assert_eq!(rec.terminal_id, "term-seam");
        assert_eq!(rec.config_dir.as_deref(), Some("C:/claude/.claude-ben"));
        assert_eq!(rec.state, "open");
        assert_eq!(rec.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(rec.confirmed_at.is_some());
        assert_eq!(
            crate::mcp::session_message_poller::bound_open_terminal_id(&rec),
            Some("term-seam")
        );
    }
}
