//! Claude-CLI transport.
//!
//! Two delivery modes share this transport:
//!
//! 1. [`SessionKind::TerminalClaude`] — a PTY-backed terminal where the
//!    operator drives `claude` interactively. Delegates to the same
//!    [`crate::terminal::TerminalManager`] the PTY transport uses; the
//!    terminal title is set to "Claude" so the frontend's xterm tab
//!    surface picks the right glyph.
//!
//! 2. [`SessionKind::Agentic`] — runner-spawned Claude CLI driven via
//!    the stream-json protocol. Today this path is owned end-to-end by
//!    `claude_session::ClaudeSession::spawn`, which requires deep
//!    workflow context (`AiSessionContext`, `FindingContext`,
//!    `ProgressContext`, pid-tracker handles). Phase 2 deliberately
//!    does **not** rewrite that wiring — see the module doc deviation
//!    note. Agentic sessions stamp a [`super::TransportHandle::ClaudeCli`]
//!    with the *upstream* task-run id once it's known; for greenfield
//!    sessions started directly from the Tauri surface we materialize a
//!    placeholder handle so the session row + outbox entry exist before
//!    the workflow executor takes over.
//!
//! Per `D:\qontinui-root\qontinui-runner\CLAUDE.md` "Claude CLI Spawning":
//! every spawn site must `env_remove("CLAUDECODE")` to keep Claude from
//! detecting a "nested session". The PTY-driven path inherits the
//! environment from `TerminalManager::create`, which already strips
//! `CLAUDECODE` via `process_helpers::cmd_no_window` on Windows + the
//! `terminal/session::spawn` env scrub on Unix. The agentic path delegates
//! to `ClaudeSession::spawn` which has the same scrub (see
//! `claude_session/session.rs:184`).

use std::sync::Arc;

use tauri::AppHandle;
use tracing::warn;

use crate::session::intent::Intent;
use crate::session::SessionKind;
use crate::terminal::TerminalManager;

use super::{Transport, TransportError, TransportHandle};

pub struct ClaudeCliTransport {
    /// PTY manager used for the [`SessionKind::TerminalClaude`] route.
    terminal_manager: Arc<TerminalManager>,
    app_handle: AppHandle,
}

impl ClaudeCliTransport {
    pub fn new(terminal_manager: Arc<TerminalManager>, app_handle: AppHandle) -> Self {
        Self {
            terminal_manager,
            app_handle,
        }
    }
}

impl Transport for ClaudeCliTransport {
    fn start(&self, intent: &Intent) -> Result<TransportHandle, TransportError> {
        match intent.kind {
            SessionKind::TerminalClaude => {
                // Spawn a PTY terminal — the operator types `claude` (or the
                // frontend pre-fills it via `terminal_write`). The title is
                // set to the session purpose so the dashboard shows what
                // it's for.
                let working_dir = intent
                    .repo
                    .clone()
                    .filter(|r| !r.is_empty())
                    .or_else(|| crate::mcp::shared::current_project_path());

                let info = self
                    .terminal_manager
                    .create(
                        Some(intent.purpose.clone()),
                        working_dir,
                        None,
                        None,
                        None,
                        self.app_handle.clone(),
                    )
                    .map_err(TransportError::Runtime)?;

                Ok(TransportHandle::Pty {
                    terminal_id: info.id,
                })
            }
            SessionKind::Agentic => {
                // Agentic sessions are owned by `claude_session::ClaudeSession`
                // + the workflow executor. Phase 2 materializes a coord
                // session-row + outbox entry; the actual subprocess spawn
                // happens via the existing path (the `task_run_id` returned
                // here is a placeholder that the workflow executor will
                // overwrite via [`crate::session::SessionRegistry::link_task_run`]
                // once it has a real id).
                let placeholder = format!("pending-{}", uuid::Uuid::new_v4());
                Ok(TransportHandle::ClaudeCli {
                    cli_session_id: placeholder,
                })
            }
            other => Err(TransportError::InvalidIntent(format!(
                "ClaudeCli transport only handles TerminalClaude / Agentic, got {:?}",
                other
            ))),
        }
    }

    fn write_input(&self, handle: &TransportHandle, bytes: &[u8]) -> Result<(), TransportError> {
        match handle {
            TransportHandle::Pty { terminal_id } => {
                let session = self.terminal_manager.get(terminal_id).ok_or_else(|| {
                    TransportError::Runtime(format!("terminal not found: {}", terminal_id))
                })?;
                session.write(bytes).map_err(TransportError::Runtime)
            }
            TransportHandle::ClaudeCli { .. } => {
                // Agentic stream-json input is routed through
                // `commands::ai_session::ai_session_send_message`; the
                // Session primitive doesn't proxy raw bytes for those.
                Err(TransportError::Unsupported(
                    "agentic write_input — route through ai_session_send_message",
                ))
            }
            TransportHandle::Workflow { .. } | TransportHandle::External => {
                Err(TransportError::Runtime(
                    "ClaudeCli transport got non-PTY/non-CLI handle".to_string(),
                ))
            }
        }
    }

    fn resize(&self, handle: &TransportHandle, cols: u16, rows: u16) -> Result<(), TransportError> {
        match handle {
            TransportHandle::Pty { terminal_id } => {
                let session = self.terminal_manager.get(terminal_id).ok_or_else(|| {
                    TransportError::Runtime(format!("terminal not found: {}", terminal_id))
                })?;
                session.resize(cols, rows).map_err(TransportError::Runtime)
            }
            // Agentic sessions don't have a user-visible terminal.
            TransportHandle::ClaudeCli { .. } => Ok(()),
            TransportHandle::Workflow { .. } | TransportHandle::External => {
                Err(TransportError::Runtime(
                    "ClaudeCli transport got non-PTY/non-CLI handle".to_string(),
                ))
            }
        }
    }

    fn close(&self, handle: &TransportHandle) -> Result<(), TransportError> {
        match handle {
            TransportHandle::Pty { terminal_id } => {
                match self.terminal_manager.close(terminal_id) {
                    Ok(()) => Ok(()),
                    Err(e) if e.contains("not found") => Ok(()),
                    Err(e) => Err(TransportError::Runtime(e)),
                }
            }
            TransportHandle::ClaudeCli { cli_session_id } => {
                // For non-placeholder ids the workflow executor's close hook
                // is the canonical teardown; the transport just records the
                // intent to close. Phase 9 collapses both paths.
                if !cli_session_id.starts_with("pending-") {
                    warn!(
                        "ClaudeCli close on real session id {} — workflow executor owns teardown",
                        cli_session_id
                    );
                }
                Ok(())
            }
            TransportHandle::Workflow { .. } | TransportHandle::External => {
                Err(TransportError::Runtime(
                    "ClaudeCli transport got non-PTY/non-CLI handle".to_string(),
                ))
            }
        }
    }
}
