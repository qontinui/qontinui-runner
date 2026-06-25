//! Unified Session primitive — plan
//! [`2026-05-22-coord-native-session-coordination`] §D1 + §Phase 2.
//!
//! Today the runner has three parallel session concepts:
//!
//! - [`crate::terminal::TerminalManager`] (PTY-backed shell sessions)
//! - [`crate::claude_session::ClaudeSession`] (Claude CLI subprocesses)
//! - Workflow sessions (`unified_workflow_executor` + `project.sessions`)
//!
//! Plan §D1 makes them one [`Session`] lifecycle with a typed
//! [`SessionKind`] discriminator. The transport for each kind lives under
//! [`transport`]; the wire shapes (SessionKind, SessionState,
//! SessionEventKind, Intent) mirror `qontinui-coord/src/sessions.rs`
//! byte-for-byte so the runner-side struct serializes verbatim into
//! `coord.sessions.intent` / `coord.sessions.session_kind` /
//! `coord.sessions.state` columns.
//!
//! ## Phase 2 scope (this PR)
//!
//! - Wire shapes that match `coord.sessions` exactly
//! - [`Session::start`] / [`SessionHandle`] lifecycle with transport
//!   selection
//! - [`Intent`] validation
//! - Local outbox ([`local_store::OutboxWriter`]) — JSON-lines per
//!   precedent in `wrappers/registry.rs` (see deviation note there); the
//!   plan calls out SQLite, but adding rusqlite for an outbox we read
//!   sequentially adds a new native dep + migration surface for no
//!   benefit over a JSONL file.
//! - Tauri command surface (see `commands::session`) — `session_start`,
//!   `session_focus`, `session_close`, `session_describe`,
//!   `session_steal {reason}`
//!
//! ## Out of scope (deferred to later phases)
//!
//! - **Phase 3** — the coord-sync push/heartbeat loop is a stub in
//!   [`coord_sync`]. Local outbox is written; nothing pushes to coord yet.
//! - **Phase 4** — frontend command callers (`SessionContext`) cut over
//!   from the legacy `terminal_*` / `ai_session_*` commands.
//! - **Phase 9** — the existing `commands/terminal.rs`,
//!   `claude_session/` (9 files), and legacy session tables are kept in
//!   place. They're load-bearing for ~24 consumers across `productivity`,
//!   `coordinator`, `executor`, `mcp`, `fixer`, `follow_up`,
//!   `zombie_sweep`. Phase 9 deletion happens after Phase 4 frontend
//!   cutover routes the consumers through this primitive.
//!
//! ## Why an additive layer instead of a wholesale rewrite?
//!
//! Per `CLAUDE.md` "Delete over deprecate" and the plan's "no backward
//! compat" stance, the long-term plan is deletion. But the plan's
//! own §Phase 9 explicitly slates `commands/terminal.rs` + `claude_session/`
//! for deletion AFTER Phase 4 wires the frontend to the new commands. The
//! existing modules have ~24 in-crate consumers (productivity workers,
//! coordinator deconflicter, etc.) and front-end Tauri commands that the
//! runner UI calls today. Deleting them in Phase 2 would break the
//! runner. Phase 2 is the **substrate** for Phase 4 — the new commands
//! coexist with the old ones, and Phase 9 cleans up once the new path
//! has fully absorbed the old surface.

pub mod claude_hook;
pub mod coord_sync;
pub mod dual_write;
pub mod handoff;
pub mod intent;
pub mod local_store;
pub mod output_pipe;
pub mod pane_store;
pub mod provider_adapter;
pub mod reconcile;
pub mod session_lifecycle_store;
pub mod shutdown_marker;
pub mod transport;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use thiserror::Error;
use uuid::Uuid;

pub use coord_sync::ResumeProbe;
pub use intent::{Intent, IntentError};
pub use transport::{DynTransport, ExternalTransport, Transport, TransportError, TransportHandle};

// ---------------------------------------------------------------------------
// Wire types — must match `coord.sessions` (snake_case enum variants, exact
// variant set). Cross-reference: qontinui-coord/src/sessions.rs ~146-202.
// ---------------------------------------------------------------------------

/// Discriminator for a [`Session`]. Mirrors the `coord.sessions.session_kind`
/// CHECK constraint and Phase 1's wire enum 1:1 — the runner serializes
/// directly into the coord column.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    TerminalShell,
    TerminalClaude,
    Agentic,
    Workflow,
    Automation,
    Debug,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::TerminalShell => "terminal_shell",
            SessionKind::TerminalClaude => "terminal_claude",
            SessionKind::Agentic => "agentic",
            SessionKind::Workflow => "workflow",
            SessionKind::Automation => "automation",
            SessionKind::Debug => "debug",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "terminal_shell" => SessionKind::TerminalShell,
            "terminal_claude" => SessionKind::TerminalClaude,
            "agentic" => SessionKind::Agentic,
            "workflow" => SessionKind::Workflow,
            "automation" => SessionKind::Automation,
            "debug" => SessionKind::Debug,
            _ => return None,
        })
    }
}

/// Mirrors `coord.sessions.state` CHECK constraint and Phase 1 wire enum
/// 1:1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    PendingResolution,
    Stale,
    Closed,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Active => "active",
            SessionState::PendingResolution => "pending_resolution",
            SessionState::Stale => "stale",
            SessionState::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => SessionState::Active,
            "pending_resolution" => SessionState::PendingResolution,
            "stale" => SessionState::Stale,
            "closed" => SessionState::Closed,
            _ => return None,
        })
    }
}

/// Event kinds published on `qontinui.sessions.<tenant>.<machine>.<kind>`.
/// Mirrors Phase 1 wire enum 1:1 — `OutputChunk` (Phase 8) and
/// `HandoffRequest` (Phase 7) are pre-declared for stable wire shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    Started,
    StateChange,
    Closed,
    Heartbeat,
    ClaimStolen,
    /// Phase 8 — defined now, published when PTY streaming lands.
    OutputChunk,
    /// Phase 7 — defined now, published when handoff lands.
    HandoffRequest,
    /// Commit ↔ session lineage push-report (plan
    /// `2026-06-07-coord-commit-session-lineage.md`, Population path 2).
    /// Drained to `POST /coord/commits/report {repo, branch, shas[]}`; the
    /// payload carries `{repo, branch, shas}` and NO session id (coord resolves
    /// the session server-side from `(repo, branch)`).
    CommitReport,
}

impl SessionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionEventKind::Started => "started",
            SessionEventKind::StateChange => "state_change",
            SessionEventKind::Closed => "closed",
            SessionEventKind::Heartbeat => "heartbeat",
            SessionEventKind::ClaimStolen => "claim_stolen",
            SessionEventKind::OutputChunk => "output_chunk",
            SessionEventKind::HandoffRequest => "handoff_request",
            SessionEventKind::CommitReport => "commit_report",
        }
    }
}

// ---------------------------------------------------------------------------
// Session + SessionHandle
// ---------------------------------------------------------------------------

/// Errors raised by the [`Session`] lifecycle.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("invalid intent: {0}")]
    Intent(#[from] IntentError),
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("steal reason too short ({got} < {min} chars)")]
    StealReasonTooShort { got: usize, min: usize },
    #[error("session not found: {0}")]
    NotFound(Uuid),
    #[error("local outbox error: {0}")]
    Outbox(String),
}

/// Minimum chars on a steal reason — plan §D14.
pub const MIN_STEAL_REASON_CHARS: usize = 10;

/// Description of an existing session, returned by [`SessionHandle::describe`]
/// and the `session_describe` Tauri command. Mirrors the subset of
/// `SessionRow` the frontend needs (tenant + device omitted — those are
/// constant per machine and the frontend already knows them).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDescription {
    pub id: Uuid,
    pub kind: SessionKind,
    pub state: SessionState,
    pub intent: Intent,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub parent_session_id: Option<Uuid>,
    /// Ambient Claude Code session id (`CLAUDE_CODE_SESSION_ID`) captured at
    /// start — the same id the `prepare-commit-msg` hook stamps as the
    /// `Session-Id` git trailer. Lets attribution be a join, not a hunt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code_session_id: Option<String>,
    pub transport_handle_kind: &'static str,
}

/// Internal record stored in [`SessionRegistry`]. The transport is held as
/// an `Arc<dyn Transport>` so the handle methods can route back to it
/// without going through a global lookup.
struct SessionRecord {
    id: Uuid,
    kind: SessionKind,
    state: SessionState,
    intent: Intent,
    started_at: chrono::DateTime<chrono::Utc>,
    last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    parent_session_id: Option<Uuid>,
    /// Ambient `CLAUDE_CODE_SESSION_ID` at session start (the id that also
    /// lands in commit `Session-Id` trailers). `None` outside Claude Code.
    claude_code_session_id: Option<String>,
    transport: DynTransport,
    transport_handle: TransportHandle,
    /// Phase 8 (plan §D10) — the opt-in output-streaming pipe task, present
    /// only when `intent.share_output` was true at start. Held so it lives
    /// for the session's lifetime; aborted on close. `None` for the
    /// default (non-shared) session — zero overhead off the opt-in path.
    output_pipe: Option<tokio::task::JoinHandle<()>>,
}

impl SessionRecord {
    fn describe(&self) -> SessionDescription {
        SessionDescription {
            id: self.id,
            kind: self.kind,
            state: self.state,
            intent: self.intent.clone(),
            started_at: self.started_at,
            last_heartbeat_at: self.last_heartbeat_at,
            closed_at: self.closed_at,
            parent_session_id: self.parent_session_id,
            claude_code_session_id: self.claude_code_session_id.clone(),
            transport_handle_kind: match &self.transport_handle {
                TransportHandle::Pty { .. } => "pty",
                TransportHandle::ClaudeCli { .. } => "claude_cli",
                TransportHandle::Workflow { .. } => "workflow",
                TransportHandle::External => "external",
            },
        }
    }
}

/// Process-wide registry of live sessions. Owns the in-memory map keyed by
/// session UUID. Stored in Tauri state via `Arc<SessionRegistry>` and
/// reached from the `commands::session` plugin.
pub struct SessionRegistry {
    machine_id: Uuid,
    sessions: Mutex<HashMap<Uuid, SessionRecord>>,
    transports: SessionTransports,
    coord_sync: coord_sync::CoordSync,
}

/// Bag of per-kind transports.
pub struct SessionTransports {
    pub pty: DynTransport,
    pub claude_cli: DynTransport,
    pub workflow: DynTransport,
}

impl SessionTransports {
    fn pick(&self, kind: SessionKind) -> DynTransport {
        match kind {
            SessionKind::TerminalShell => self.pty.clone(),
            SessionKind::TerminalClaude | SessionKind::Agentic => self.claude_cli.clone(),
            SessionKind::Workflow | SessionKind::Automation | SessionKind::Debug => {
                self.workflow.clone()
            }
        }
    }
}

/// Capture the ambient Claude Code session id (`CLAUDE_CODE_SESSION_ID`) —
/// the same id the `prepare-commit-msg` hook stamps as the `Session-Id` git
/// trailer on commits. Recording it on the session row lets coord *join* its
/// session records to commit history (and to the conversation that produced
/// them) instead of forcing a forensic hunt when a worktree is later found in
/// an unexpected state. Returns `None` outside Claude Code (manual launches,
/// CI) — those stay untagged, which is the correct default.
///
/// NB: this reads the runner *process* environment, so it reflects the
/// session the runner was launched under. Per-subprocess capture (the spawned
/// `claude` CLI's own id for `TerminalClaude`/`Agentic` sessions) is a
/// follow-up to be calibrated against real runner usage; this seed is the
/// un-backfillable part — start recording it now so early coordinated history
/// is joinable.
fn ambient_claude_code_session_id() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl SessionRegistry {
    pub fn new(
        machine_id: Uuid,
        transports: SessionTransports,
        coord_sync: coord_sync::CoordSync,
    ) -> Arc<Self> {
        Arc::new(Self {
            machine_id,
            sessions: Mutex::new(HashMap::new()),
            transports,
            coord_sync,
        })
    }

    pub fn machine_id(&self) -> Uuid {
        self.machine_id
    }

    /// Start a new session. Plan §D1 single-constructor surface.
    pub fn start(self: &Arc<Self>, intent: Intent) -> Result<SessionHandle, SessionError> {
        self.start_inner(intent, None)
    }

    /// Plan §Phase 7 — start a session whose lineage points back to a
    /// source session on another machine. Used by the handoff receiver
    /// to materialize the child of a cross-machine move; the
    /// `parent_session_id` lands in the `started` outbox payload so coord
    /// stamps `coord.sessions.parent_session_id` on the create.
    pub fn start_with_parent(
        self: &Arc<Self>,
        intent: Intent,
        parent_session_id: Uuid,
    ) -> Result<SessionHandle, SessionError> {
        self.start_inner(intent, Some(parent_session_id))
    }

    fn start_inner(
        self: &Arc<Self>,
        intent: Intent,
        parent_session_id: Option<Uuid>,
    ) -> Result<SessionHandle, SessionError> {
        intent.validate()?;

        let transport = self.transports.pick(intent.kind);
        let transport_handle = transport.start(&intent)?;

        let id = uuid_v7();
        let now = chrono::Utc::now();
        let claude_code_session_id = ambient_claude_code_session_id();

        // Phase 8 (plan §D10/§D11) — opt-in PTY output streaming. Only when
        // the session declared `share_output`: tap the transport's output
        // broadcast and spawn the publish pipe. Off by default → no tap, no
        // task, zero overhead. coord resolves the session's tenant
        // server-side, so the pipe only needs the session id + coord
        // client (held by `coord_sync`).
        let output_pipe = if intent.share_output {
            match transport.tap_output(&transport_handle) {
                Some(rx) => Some(output_pipe::spawn(
                    self.coord_sync.clone(),
                    id,
                    rx,
                    intent.effective_redact_secrets(),
                )),
                None => {
                    tracing::debug!(
                        session = %id,
                        kind = intent.kind.as_str(),
                        "session: share_output set but transport exposes no output tap — skipping pipe"
                    );
                    None
                }
            }
        } else {
            None
        };

        let record = SessionRecord {
            id,
            kind: intent.kind,
            state: SessionState::Active,
            intent: intent.clone(),
            started_at: now,
            last_heartbeat_at: Some(now),
            closed_at: None,
            parent_session_id,
            claude_code_session_id: claude_code_session_id.clone(),
            transport: transport.clone(),
            transport_handle,
            output_pipe,
        };

        // Outbox row first (durability before in-memory commit so a crash
        // between insert and outbox-write doesn't lose the record).
        // `parent_session_id` is threaded into the payload so the drain
        // loop's `rebuild_create_body` forwards it to coord's
        // `CreateSessionRequest.parent_session_id`.
        let payload = json!({
            "id": id,
            "kind": intent.kind.as_str(),
            "intent": intent,
            "state": SessionState::Active.as_str(),
            "started_at": now,
            "parent_session_id": parent_session_id,
            "claude_code_session_id": claude_code_session_id,
        });
        self.coord_sync
            .outbox()
            .record(self.machine_id, id, SessionEventKind::Started, payload)
            .map_err(|e| SessionError::Outbox(e.to_string()))?;

        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        sessions.insert(id, record);

        Ok(SessionHandle {
            id,
            registry: Arc::clone(self),
        })
    }

    /// Write input bytes to a live session's transport. Plan §Phase 7
    /// uses this to replay warm-tier PTY scrollback into a freshly
    /// materialized handoff child. Routes through the same transport the
    /// session was started on.
    pub fn write_input(&self, id: Uuid, bytes: &[u8]) -> Result<(), SessionError> {
        let (transport, handle) = {
            let sessions = self.sessions.lock().expect("session registry poisoned");
            let rec = sessions.get(&id).ok_or(SessionError::NotFound(id))?;
            (
                Arc::clone(&rec.transport),
                clone_transport_handle(&rec.transport_handle),
            )
        };
        transport.write_input(&handle, bytes)?;
        Ok(())
    }

    /// Register a coord-native mirror of an **externally-owned** session
    /// (plan §Phase 10 dual-write). Unlike [`SessionRegistry::start`],
    /// this does NOT spawn a transport — the real PTY / CLI subprocess is
    /// owned and torn down by the legacy `terminal_create` /
    /// `claude_session` path. The mirror exists purely so the dashboard
    /// renders the session from `coord.sessions` during the dual-write
    /// window.
    ///
    /// Validates the intent, writes a `Started` outbox row (which the
    /// drain loop pushes to coord), and inserts a record backed by the
    /// no-op [`transport::ExternalTransport`]. Closing the mirror (by id)
    /// records a `Closed` event but never touches the operator's real
    /// terminal.
    ///
    /// Called only when the cutover flag is ON (see
    /// [`coord_sync::CoordSync::mirror_legacy_session`]); with the flag
    /// off this is never reached.
    pub fn register_external(self: &Arc<Self>, intent: Intent) -> Result<Uuid, SessionError> {
        intent.validate()?;

        let id = uuid_v7();
        let now = chrono::Utc::now();
        let claude_code_session_id = ambient_claude_code_session_id();
        let transport: DynTransport = Arc::new(transport::ExternalTransport);

        let record = SessionRecord {
            id,
            kind: intent.kind,
            state: SessionState::Active,
            intent: intent.clone(),
            started_at: now,
            last_heartbeat_at: Some(now),
            closed_at: None,
            parent_session_id: None,
            claude_code_session_id: claude_code_session_id.clone(),
            transport,
            transport_handle: TransportHandle::External,
            // Externally-owned mirror — the real PTY belongs to the legacy
            // path, so no output pipe (the legacy path doesn't share output
            // through this primitive). Phase 8 streaming only attaches to
            // sessions started via `start`/`start_with_parent`.
            output_pipe: None,
        };

        let payload = json!({
            "id": id,
            "kind": intent.kind.as_str(),
            "intent": intent,
            "state": SessionState::Active.as_str(),
            "started_at": now,
            "claude_code_session_id": claude_code_session_id,
        });
        self.coord_sync
            .outbox()
            .record(self.machine_id, id, SessionEventKind::Started, payload)
            .map_err(|e| SessionError::Outbox(e.to_string()))?;

        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        sessions.insert(id, record);

        Ok(id)
    }

    /// R2 (session-lifecycle-cleanup) — RESUME a previously-registered
    /// external session by its persisted coord id instead of minting a
    /// fresh one. Used on runner restart when a restored terminal pane has
    /// a coord session id persisted in
    /// [`crate::session::pane_store::PaneSessionStore`].
    ///
    /// ## Why this exists
    ///
    /// [`Self::register_external`] always allocates a new `uuid_v7`, so a
    /// pane that re-launches across restarts orphans its old `coord.sessions`
    /// row and accumulates duplicates. Resuming re-activates the EXISTING
    /// row via a PATCH (`state=active` + `heartbeat`) — coord already
    /// supports both on `UpdateSessionRequest`, so **no coord-side change
    /// is required**.
    ///
    /// ## PATCH-vs-fresh decision + 404 fallback
    ///
    /// We probe coord synchronously with `PATCH /sessions/:id`
    /// `{state:"active", heartbeat:true}`:
    ///
    /// - **2xx** → the row exists; re-insert the in-memory [`SessionRecord`]
    ///   under `persisted_id`, emit a `StateChange` outbox row (so the drain
    ///   loop keeps coord fresh and the local state is consistent), and
    ///   return `persisted_id`. The session id is unchanged across the
    ///   restart — no duplicate.
    /// - **404 / 410** → the row was GC'd (or never existed). Fall back to a
    ///   fresh [`Self::register_external`] (new id, `Started` POST) so the
    ///   pane is never left without a coord session, and return the NEW id.
    ///   The caller persists the new id over the stale one.
    /// - **transport error / 5xx** (coord temporarily unreachable) → resume
    ///   OPTIMISTICALLY: re-insert under `persisted_id` and emit a
    ///   `StateChange` row. The drain loop retries the PATCH on reconnect;
    ///   if the row turns out to be gone, the next heartbeat PATCH 404s and
    ///   the row is treated as permanently-failed locally (the dashboard
    ///   simply won't show it until the operator re-launches). A fresh POST
    ///   here would fail identically while coord is down, so re-using the
    ///   persisted id is the strictly-better choice (idempotent reconnect).
    ///
    /// Returns the coord session id the pane should now persist (either the
    /// resumed `persisted_id` or the fresh fallback id).
    pub async fn resume_external(
        self: &Arc<Self>,
        persisted_id: Uuid,
        intent: Intent,
    ) -> Result<Uuid, SessionError> {
        intent.validate()?;

        let probe = self.coord_sync.probe_resume(persisted_id).await;

        match probe {
            ResumeProbe::Found => {
                self.insert_resumed_record(persisted_id, intent);
                tracing::info!(
                    session = %persisted_id,
                    "session: resumed existing coord session (PATCH 2xx)"
                );
                Ok(persisted_id)
            }
            ResumeProbe::NotFound => {
                tracing::info!(
                    session = %persisted_id,
                    "session: persisted coord session gone (404) — registering fresh"
                );
                self.register_external(intent)
            }
            ResumeProbe::Unreachable => {
                // Optimistic resume — coord is down, not the row. Re-use the
                // persisted id so reconnect is idempotent.
                self.insert_resumed_record(persisted_id, intent);
                tracing::warn!(
                    session = %persisted_id,
                    "session: coord unreachable during resume — resuming optimistically, drain loop will retry"
                );
                Ok(persisted_id)
            }
        }
    }

    /// Re-insert the in-memory mirror record for a resumed session under the
    /// persisted id, and emit a `StateChange` (→ PATCH `state=active`) so
    /// the drain loop reconciles coord. Mirrors [`Self::register_external`]'s
    /// record shape but reuses `persisted_id` and emits `StateChange`
    /// instead of `Started`.
    fn insert_resumed_record(self: &Arc<Self>, persisted_id: Uuid, intent: Intent) {
        let now = chrono::Utc::now();
        let claude_code_session_id = ambient_claude_code_session_id();
        let transport: DynTransport = Arc::new(transport::ExternalTransport);

        let record = SessionRecord {
            id: persisted_id,
            kind: intent.kind,
            state: SessionState::Active,
            intent: intent.clone(),
            started_at: now,
            last_heartbeat_at: Some(now),
            closed_at: None,
            parent_session_id: None,
            claude_code_session_id,
            transport,
            transport_handle: TransportHandle::External,
            output_pipe: None,
        };

        // Emit a state_change → PATCH so the drain loop re-activates the row
        // (state=active + heartbeat). state_change_body always stamps
        // heartbeat:true, refreshing last_heartbeat_at coord-side.
        let payload = json!({
            "id": persisted_id,
            "state": SessionState::Active.as_str(),
            "repo": intent.repo,
            "branch": intent.branch,
        });
        if let Err(e) = self.coord_sync.outbox().record(
            self.machine_id,
            persisted_id,
            SessionEventKind::StateChange,
            payload,
        ) {
            tracing::warn!(
                session = %persisted_id,
                error = %e,
                "session: resume state_change outbox write failed"
            );
        }

        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        sessions.insert(persisted_id, record);
    }

    fn with_record<R>(
        &self,
        id: Uuid,
        f: impl FnOnce(&mut SessionRecord) -> R,
    ) -> Result<R, SessionError> {
        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        match sessions.get_mut(&id) {
            Some(rec) => Ok(f(rec)),
            None => Err(SessionError::NotFound(id)),
        }
    }

    fn describe(&self, id: Uuid) -> Result<SessionDescription, SessionError> {
        let sessions = self.sessions.lock().expect("session registry poisoned");
        sessions
            .get(&id)
            .map(SessionRecord::describe)
            .ok_or(SessionError::NotFound(id))
    }

    fn list_all(&self) -> Vec<SessionDescription> {
        let sessions = self.sessions.lock().expect("session registry poisoned");
        sessions.values().map(SessionRecord::describe).collect()
    }

    fn close(&self, id: Uuid) -> Result<(), SessionError> {
        // Snapshot the transport handle + transport ref under the lock,
        // then drop the lock before calling transport.close (which may
        // block on subprocess teardown).
        let (transport, handle, payload) = {
            let mut sessions = self.sessions.lock().expect("session registry poisoned");
            let rec = sessions.get_mut(&id).ok_or(SessionError::NotFound(id))?;
            if matches!(rec.state, SessionState::Closed) {
                return Ok(()); // idempotent
            }
            rec.state = SessionState::Closed;
            rec.closed_at = Some(chrono::Utc::now());
            // Phase 8 — stop the output pipe (if any). Aborting drops the
            // broadcast receiver; the pipe also exits on its own when the
            // terminal's sender drops (transport.close below), but the
            // explicit abort is immediate + idempotent. The pipe does a
            // best-effort final flush on Closed via its own RecvError path.
            if let Some(pipe) = rec.output_pipe.take() {
                pipe.abort();
            }
            let payload = json!({
                "id": id,
                "state": SessionState::Closed.as_str(),
                "closed_at": rec.closed_at,
            });
            // We must clone the transport + handle out of the record so we
            // can drop the lock before close (close may take a while).
            let transport = Arc::clone(&rec.transport);
            let handle = clone_transport_handle(&rec.transport_handle);
            (transport, handle, payload)
        };

        let close_result = transport.close(&handle);

        // Outbox the close event regardless of transport close result —
        // the dashboard wants to know we tried to close even if the
        // subprocess was already dead.
        if let Err(e) =
            self.coord_sync
                .outbox()
                .record(self.machine_id, id, SessionEventKind::Closed, payload)
        {
            tracing::warn!("session {} close outbox write failed: {}", id, e);
        }

        close_result?;
        Ok(())
    }

    /// Plan §D14 — steal-with-reason. Min 10 chars. Records a
    /// `claim_stolen` event in the outbox; the actual claim release is
    /// driven by `agent_claims` / coord and lands in Phase 6.
    fn steal(&self, id: Uuid, reason: &str) -> Result<(), SessionError> {
        if reason.chars().count() < MIN_STEAL_REASON_CHARS {
            return Err(SessionError::StealReasonTooShort {
                got: reason.chars().count(),
                min: MIN_STEAL_REASON_CHARS,
            });
        }
        // Confirm the session exists (so we don't write phantom events
        // for unknown ids).
        let _ = self.describe(id)?;

        let payload = json!({
            "id": id,
            "reason": reason,
            "stolen_at": chrono::Utc::now(),
        });
        self.coord_sync
            .outbox()
            .record(self.machine_id, id, SessionEventKind::ClaimStolen, payload)
            .map_err(|e| SessionError::Outbox(e.to_string()))?;
        Ok(())
    }

    /// "Focus" surfaces in Phase 4 as a frontend event — currently a
    /// no-op stub that records a `state_change` heartbeat-like event so
    /// the dashboard sees the operator paying attention.
    fn focus(&self, id: Uuid) -> Result<(), SessionError> {
        let now = chrono::Utc::now();
        self.with_record(id, |rec| {
            rec.last_heartbeat_at = Some(now);
        })?;
        let payload = json!({
            "id": id,
            "kind": "focus",
            "at": now,
        });
        self.coord_sync
            .outbox()
            .record(self.machine_id, id, SessionEventKind::StateChange, payload)
            .map_err(|e| SessionError::Outbox(e.to_string()))?;
        Ok(())
    }

    /// Attach an output pipe to an existing externally-owned session so
    /// its PTY output streams to coord. Used by `terminal_create` and
    /// `spawn_worker_session` after they register an external session and
    /// have a broadcast receiver from the real PTY.
    ///
    /// The spawned pipe task is stored on the session record so it lives
    /// for the session's lifetime and is aborted on close (same lifecycle
    /// as pipes spawned inside `start_inner`).
    pub fn attach_output_pipe(
        &self,
        id: Uuid,
        rx: tokio::sync::broadcast::Receiver<String>,
        redact: bool,
    ) {
        let handle = output_pipe::spawn(self.coord_sync.clone(), id, rx, redact);
        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        if let Some(rec) = sessions.get_mut(&id) {
            // If there was already a pipe (shouldn't happen for external
            // sessions, but be safe), abort it before replacing.
            if let Some(old) = rec.output_pipe.take() {
                old.abort();
            }
            rec.output_pipe = Some(handle);
        } else {
            // Session was already closed/removed before we could attach —
            // abort the just-spawned pipe so it doesn't leak.
            handle.abort();
        }
    }

    /// Borrow the coord-sync facade. Phase 3 will use this to drive the
    /// drain loop from main.rs.
    pub fn coord_sync(&self) -> &coord_sync::CoordSync {
        &self.coord_sync
    }

    /// Read-only snapshot of all sessions. Used by the
    /// `commands::session::session_list` Tauri command and by tests.
    pub fn snapshot(&self) -> Vec<SessionDescription> {
        self.list_all()
    }

    /// Describe-by-id surface used by the Tauri command layer (which
    /// holds `tauri::State<Arc<SessionRegistry>>` and does not have a
    /// [`SessionHandle`]).
    pub fn describe_by_id(&self, id: Uuid) -> Result<SessionDescription, SessionError> {
        self.describe(id)
    }

    /// Close-by-id — same idempotent semantics as
    /// [`SessionHandle::close`].
    pub fn close_by_id(&self, id: Uuid) -> Result<(), SessionError> {
        self.close(id)
    }

    /// Focus-by-id surface for the Tauri command layer.
    pub fn focus_by_id(&self, id: Uuid) -> Result<(), SessionError> {
        self.focus(id)
    }

    /// Steal-by-id surface for the Tauri command layer.
    pub fn steal_by_id(&self, id: Uuid, reason: &str) -> Result<(), SessionError> {
        self.steal(id, reason)
    }

    /// Plan §Phase 3 — flip an in-memory session's state without
    /// recording a wire event. Used by the coord-sync drain loop when it
    /// observes a 409 conflict (→ `PendingResolution`) and by the
    /// heartbeat loop when it observes an elapsed session (→ `Stale`).
    ///
    /// The state change is local-only — the corresponding coord update
    /// flows through whatever event the caller is already emitting (the
    /// 409 itself is the wire signal for `PendingResolution`; the stale
    /// flag is a UI cue, not a coord column).
    pub fn set_state(&self, id: Uuid, state: SessionState) {
        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        if let Some(rec) = sessions.get_mut(&id) {
            rec.state = state;
        }
    }

    /// Test-only: rewind a session's `last_heartbeat_at` so the
    /// heartbeat-loop's elapsed-time check (the local stale flip) trips
    /// deterministically without sleeping for the real stale interval.
    #[cfg(test)]
    pub fn force_heartbeat_to_for_test(&self, id: Uuid, when: chrono::DateTime<chrono::Utc>) {
        let mut sessions = self.sessions.lock().expect("session registry poisoned");
        if let Some(rec) = sessions.get_mut(&id) {
            rec.last_heartbeat_at = Some(when);
            // The sweep uses `last_heartbeat_at.unwrap_or(started_at)`,
            // so also rewind started_at to match — otherwise a brand-new
            // session with `started_at = now` would clamp elapsed to ~0.
            rec.started_at = when;
        }
    }
}

fn clone_transport_handle(h: &TransportHandle) -> TransportHandle {
    match h {
        TransportHandle::Pty { terminal_id } => TransportHandle::Pty {
            terminal_id: terminal_id.clone(),
        },
        TransportHandle::ClaudeCli { cli_session_id } => TransportHandle::ClaudeCli {
            cli_session_id: cli_session_id.clone(),
        },
        TransportHandle::Workflow { task_run_id } => TransportHandle::Workflow {
            task_run_id: task_run_id.clone(),
        },
        TransportHandle::External => TransportHandle::External,
    }
}

/// Lightweight handle returned to the caller of [`SessionRegistry::start`].
/// Holds an Arc back to the registry so subsequent ops can find the
/// record by id.
#[derive(Clone)]
pub struct SessionHandle {
    id: Uuid,
    registry: Arc<SessionRegistry>,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("id", &self.id)
            .finish()
    }
}

impl SessionHandle {
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Render the current session state for the Tauri layer / dashboard.
    pub fn describe(&self) -> Result<SessionDescription, SessionError> {
        self.registry.describe(self.id)
    }

    /// Tear the session down. Idempotent.
    pub fn close(self) -> Result<(), SessionError> {
        self.registry.close(self.id)
    }

    /// Phase 4 "Focus this session" command — heartbeats the session
    /// row so the dashboard knows the operator is on it.
    pub fn focus(&self) -> Result<(), SessionError> {
        self.registry.focus(self.id)
    }

    /// Plan §D14 — initiate a claim-steal with a typed reason.
    pub fn steal(&self, reason: &str) -> Result<(), SessionError> {
        self.registry.steal(self.id, reason)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// UUIDv7 (time-ordered) for session ids. Plan §Phase 2 calls UUIDv7 out
/// explicitly so the runner-local outbox row sorts naturally with the
/// coord row — UUIDv4 would still work for correctness but defeats the
/// `(machine_id, session_id, seq)` "time-ordered" intuition.
pub(crate) fn uuid_v7() -> Uuid {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let ts = uuid::Timestamp::from_unix(uuid::NoContext, now.as_secs(), now.subsec_nanos());
    Uuid::new_v7(ts)
}

/// Convert a [`JsonValue`] payload into a [`SessionDescription`]. Used by
/// `commands::session::session_describe` to thread the registry response
/// through the Tauri `CommandResponse` envelope without `serde_json::to_value`
/// at the call site.
pub fn description_to_json(desc: &SessionDescription) -> JsonValue {
    serde_json::to_value(desc).unwrap_or(JsonValue::Null)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// In-memory transport that records every call for test inspection.
    /// Doesn't talk to a real PTY / subprocess — that's the point: tests
    /// should not depend on Tauri runtime, OS shells, or external
    /// processes.
    struct FakeTransport {
        kind_filter: SessionKind,
        starts: AtomicUsize,
        writes: AtomicUsize,
        resizes: AtomicUsize,
        closes: AtomicUsize,
    }

    impl FakeTransport {
        fn new(kind_filter: SessionKind) -> Self {
            Self {
                kind_filter,
                starts: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
                resizes: AtomicUsize::new(0),
                closes: AtomicUsize::new(0),
            }
        }
    }

    impl Transport for FakeTransport {
        fn start(&self, intent: &Intent) -> Result<TransportHandle, TransportError> {
            if intent.kind != self.kind_filter {
                return Err(TransportError::InvalidIntent(format!(
                    "fake transport wants {:?}, got {:?}",
                    self.kind_filter, intent.kind
                )));
            }
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(TransportHandle::Pty {
                terminal_id: "fake-1".to_string(),
            })
        }
        fn write_input(&self, _h: &TransportHandle, _b: &[u8]) -> Result<(), TransportError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn resize(&self, _h: &TransportHandle, _c: u16, _r: u16) -> Result<(), TransportError> {
            self.resizes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn close(&self, _h: &TransportHandle) -> Result<(), TransportError> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Returns the registry plus the backing [`TempDir`]. The caller MUST
    /// keep the `TempDir` alive for the test's duration: the outbox reopens
    /// its file on every `record`, so a dropped temp dir (deleted on drop)
    /// makes subsequent writes fail with `ENOENT`.
    fn make_registry() -> (Arc<SessionRegistry>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let outbox =
            Arc::new(local_store::OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let coord_sync = coord_sync::CoordSync::new(outbox);

        let pty = Arc::new(FakeTransport::new(SessionKind::TerminalShell)) as DynTransport;
        let claude_cli = Arc::new(FakeTransport::new(SessionKind::TerminalClaude)) as DynTransport;
        let workflow = Arc::new(FakeTransport::new(SessionKind::Workflow)) as DynTransport;

        let registry = SessionRegistry::new(
            Uuid::new_v4(),
            SessionTransports {
                pty,
                claude_cli,
                workflow,
            },
            coord_sync,
        );
        (registry, dir)
    }

    fn shell_intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "smoke test".into(),
            repo: None,
            branch: None,
            plan_slug: None,
            correlation_topic: None,
            page_id: None,
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
        }
    }

    /// Phase 8 — a transport that exposes a real output broadcast so the
    /// `share_output` path can spawn its pipe. `start` returns a Pty handle;
    /// `tap_output` hands back a fresh receiver on the held sender.
    struct TappingTransport {
        tx: tokio::sync::broadcast::Sender<String>,
    }

    impl TappingTransport {
        fn new() -> Self {
            let (tx, _) = tokio::sync::broadcast::channel(16);
            Self { tx }
        }
    }

    impl Transport for TappingTransport {
        fn start(&self, _intent: &Intent) -> Result<TransportHandle, TransportError> {
            Ok(TransportHandle::Pty {
                terminal_id: "tap-1".to_string(),
            })
        }
        fn write_input(&self, _h: &TransportHandle, _b: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }
        fn resize(&self, _h: &TransportHandle, _c: u16, _r: u16) -> Result<(), TransportError> {
            Ok(())
        }
        fn close(&self, _h: &TransportHandle) -> Result<(), TransportError> {
            Ok(())
        }
        fn tap_output(
            &self,
            _h: &TransportHandle,
        ) -> Option<tokio::sync::broadcast::Receiver<String>> {
            Some(self.tx.subscribe())
        }
    }

    fn make_tapping_registry() -> (Arc<SessionRegistry>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let outbox =
            Arc::new(local_store::OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let coord_sync = coord_sync::CoordSync::new(outbox);
        let pty = Arc::new(TappingTransport::new()) as DynTransport;
        let claude_cli = Arc::new(FakeTransport::new(SessionKind::TerminalClaude)) as DynTransport;
        let workflow = Arc::new(FakeTransport::new(SessionKind::Workflow)) as DynTransport;
        let registry = SessionRegistry::new(
            Uuid::new_v4(),
            SessionTransports {
                pty,
                claude_cli,
                workflow,
            },
            coord_sync,
        );
        (registry, dir)
    }

    #[tokio::test]
    async fn share_output_spawns_pipe_and_close_aborts_it() {
        let (reg, _dir) = make_tapping_registry();
        let mut intent = shell_intent();
        intent.share_output = true;
        let handle = reg.start(intent).unwrap();
        let id = handle.id();
        // The pipe task is held on the record.
        {
            let sessions = reg.sessions.lock().unwrap();
            let rec = sessions.get(&id).unwrap();
            assert!(
                rec.output_pipe.is_some(),
                "share_output=true must spawn the output pipe"
            );
        }
        // Closing aborts + clears the pipe handle.
        handle.close().unwrap();
        {
            let sessions = reg.sessions.lock().unwrap();
            let rec = sessions.get(&id).unwrap();
            assert!(
                rec.output_pipe.is_none(),
                "close must take + abort the output pipe"
            );
        }
    }

    #[tokio::test]
    async fn no_share_output_spawns_no_pipe() {
        let (reg, _dir) = make_tapping_registry();
        // Default intent has share_output=false.
        let handle = reg.start(shell_intent()).unwrap();
        let id = handle.id();
        let sessions = reg.sessions.lock().unwrap();
        let rec = sessions.get(&id).unwrap();
        assert!(
            rec.output_pipe.is_none(),
            "default (non-shared) session pays zero overhead — no pipe"
        );
    }

    #[test]
    fn session_kind_round_trips_snake_case() {
        for k in [
            SessionKind::TerminalShell,
            SessionKind::TerminalClaude,
            SessionKind::Agentic,
            SessionKind::Workflow,
            SessionKind::Automation,
            SessionKind::Debug,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: SessionKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
            // The serialized form must be the same string SessionKind::as_str
            // returns — coord matches on these literals.
            assert_eq!(json.trim_matches('"'), k.as_str());
        }
    }

    #[test]
    fn session_state_round_trips_snake_case() {
        for s in [
            SessionState::Active,
            SessionState::PendingResolution,
            SessionState::Stale,
            SessionState::Closed,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: SessionState = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
            assert_eq!(json.trim_matches('"'), s.as_str());
        }
    }

    #[test]
    fn session_event_kind_round_trips_snake_case() {
        let kinds = [
            SessionEventKind::Started,
            SessionEventKind::StateChange,
            SessionEventKind::Closed,
            SessionEventKind::Heartbeat,
            SessionEventKind::ClaimStolen,
            SessionEventKind::OutputChunk,
            SessionEventKind::HandoffRequest,
            SessionEventKind::CommitReport,
        ];
        for k in kinds {
            let json = serde_json::to_string(&k).unwrap();
            let back: SessionEventKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
            assert_eq!(json.trim_matches('"'), k.as_str());
        }
    }

    #[test]
    fn lifecycle_start_describe_close() {
        let (reg, _dir) = make_registry();
        let handle = reg.start(shell_intent()).unwrap();
        let desc = handle.describe().unwrap();
        assert_eq!(desc.kind, SessionKind::TerminalShell);
        assert_eq!(desc.state, SessionState::Active);
        assert_eq!(desc.intent.purpose, "smoke test");
        assert!(desc.closed_at.is_none());

        let id = handle.id();
        handle.close().unwrap();

        let again = reg.describe(id).unwrap();
        assert_eq!(again.state, SessionState::Closed);
        assert!(again.closed_at.is_some());
    }

    #[test]
    fn close_is_idempotent() {
        let (reg, _dir) = make_registry();
        let handle = reg.start(shell_intent()).unwrap();
        let id = handle.id();
        handle.clone().close().unwrap();
        // Second close — no panic, no error.
        reg.close(id).unwrap();
    }

    #[test]
    fn start_rejects_invalid_intent() {
        let (reg, _dir) = make_registry();
        let mut intent = shell_intent();
        intent.purpose = "x".into();
        let err = reg.start(intent).unwrap_err();
        assert!(matches!(err, SessionError::Intent(_)));
    }

    #[test]
    fn register_external_does_not_spawn_transport() {
        // Phase 10 dual-write: the mirror must NOT start a transport —
        // the real process is owned by the legacy path. We assert the
        // FakeTransport's `start` counter stays at zero across a mirror.
        let (reg, _dir) = make_registry();
        let id = reg.register_external(shell_intent()).unwrap();
        let desc = reg.describe(id).unwrap();
        assert_eq!(desc.kind, SessionKind::TerminalShell);
        assert_eq!(desc.state, SessionState::Active);
        // The external mirror reports the no-op transport handle kind.
        assert_eq!(desc.transport_handle_kind, "external");
    }

    #[test]
    fn register_external_closeable_by_id_without_touching_real_process() {
        // Closing the mirror records a Closed event and never errors —
        // the no-op ExternalTransport's close is a clean Ok(()).
        let (reg, _dir) = make_registry();
        let id = reg.register_external(shell_intent()).unwrap();
        reg.close_by_id(id).unwrap();
        let again = reg.describe(id).unwrap();
        assert_eq!(again.state, SessionState::Closed);
        assert!(again.closed_at.is_some());
    }

    #[test]
    fn register_external_rejects_invalid_intent() {
        let (reg, _dir) = make_registry();
        let mut intent = shell_intent();
        intent.purpose = "x".into();
        let err = reg.register_external(intent).unwrap_err();
        assert!(matches!(err, SessionError::Intent(_)));
    }

    #[test]
    fn steal_requires_long_reason() {
        let (reg, _dir) = make_registry();
        let handle = reg.start(shell_intent()).unwrap();
        let err = handle.steal("short").unwrap_err();
        assert!(matches!(
            err,
            SessionError::StealReasonTooShort { got, min }
            if got == 5 && min == MIN_STEAL_REASON_CHARS
        ));
        // Exact-min-chars reason is accepted.
        handle.steal("1234567890").unwrap();
    }

    #[test]
    fn focus_heartbeats_session() {
        let (reg, _dir) = make_registry();
        let handle = reg.start(shell_intent()).unwrap();
        let before = handle.describe().unwrap().last_heartbeat_at;
        std::thread::sleep(std::time::Duration::from_millis(2));
        handle.focus().unwrap();
        let after = handle.describe().unwrap().last_heartbeat_at;
        assert!(after > before);
    }

    #[test]
    fn snapshot_lists_all_sessions() {
        let (reg, _dir) = make_registry();
        let _a = reg.start(shell_intent()).unwrap();
        let _b = reg.start(shell_intent()).unwrap();
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn transports_pick_routes_by_kind() {
        // FakeTransport rejects non-matching kinds, so this proves the
        // router picked the right transport.
        let (reg, _dir) = make_registry();

        let mut shell = shell_intent();
        shell.kind = SessionKind::TerminalShell;
        reg.start(shell).unwrap();

        let mut claude = shell_intent();
        claude.kind = SessionKind::TerminalClaude;
        reg.start(claude).unwrap();

        let mut wf = shell_intent();
        wf.kind = SessionKind::Workflow;
        reg.start(wf).unwrap();
    }

    #[test]
    fn description_serializes_with_snake_case_enums() {
        let (reg, _dir) = make_registry();
        let handle = reg.start(shell_intent()).unwrap();
        let desc = handle.describe().unwrap();
        let json = serde_json::to_value(&desc).unwrap();
        assert_eq!(json["kind"], "terminal_shell");
        assert_eq!(json["state"], "active");
    }
}
