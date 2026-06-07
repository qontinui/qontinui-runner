//! PTY transport — delegates to [`crate::terminal::TerminalManager`].
//!
//! Routes [`super::Transport`] calls into the existing PTY infrastructure
//! (`portable-pty`-backed `TerminalSession`s). Phase 9 cleanup will
//! collapse `commands/terminal.rs` and parts of `terminal/manager.rs`
//! into this transport; during Phase 2 we delegate so the existing PTY
//! infra stays operable for the legacy `terminal_*` Tauri command
//! surface that's still in use.

use std::sync::Arc;

use tauri::AppHandle;
use tracing::warn;

use crate::session::intent::Intent;
use crate::session::SessionKind;
use crate::terminal::TerminalManager;

use super::{Transport, TransportError, TransportHandle};

/// PTY-backed transport. Holds the Tauri-managed `Arc<TerminalManager>`
/// and an `AppHandle` so `start` can spawn new terminals (the
/// `TerminalManager::create` API needs the handle to emit
/// `terminal-created`).
pub struct PtyTransport {
    manager: Arc<TerminalManager>,
    app_handle: AppHandle,
}

impl PtyTransport {
    pub fn new(manager: Arc<TerminalManager>, app_handle: AppHandle) -> Self {
        Self {
            manager,
            app_handle,
        }
    }

    fn handle_terminal_id<'a>(
        &self,
        handle: &'a TransportHandle,
    ) -> Result<&'a str, TransportError> {
        match handle {
            TransportHandle::Pty { terminal_id } => Ok(terminal_id.as_str()),
            other => Err(TransportError::Runtime(format!(
                "PTY transport got non-PTY handle: {:?}",
                other
            ))),
        }
    }
}

impl Transport for PtyTransport {
    fn start(&self, intent: &Intent) -> Result<TransportHandle, TransportError> {
        // PTY transport carries TerminalShell. TerminalClaude also uses a
        // PTY but with a `claude` command pre-typed — that's claude_cli's
        // job; reject the wrong kind early.
        if !matches!(intent.kind, SessionKind::TerminalShell) {
            return Err(TransportError::InvalidIntent(format!(
                "PTY transport only handles TerminalShell, got {:?}",
                intent.kind
            )));
        }

        let working_dir = intent
            .repo
            .clone()
            .filter(|r| !r.is_empty())
            .or_else(|| crate::mcp::shared::current_project_path());

        let title = Some(intent.purpose.clone());

        let info = self
            .manager
            .create(
                title,
                working_dir,
                None,
                None,
                None,
                self.app_handle.clone(),
                None,
                None,
            )
            .map_err(TransportError::Runtime)?;

        Ok(TransportHandle::Pty {
            terminal_id: info.id,
        })
    }

    fn write_input(&self, handle: &TransportHandle, bytes: &[u8]) -> Result<(), TransportError> {
        let id = self.handle_terminal_id(handle)?;
        let session = self
            .manager
            .get(id)
            .ok_or_else(|| TransportError::Runtime(format!("terminal not found: {}", id)))?;
        session.write(bytes).map_err(TransportError::Runtime)
    }

    fn resize(&self, handle: &TransportHandle, cols: u16, rows: u16) -> Result<(), TransportError> {
        let id = self.handle_terminal_id(handle)?;
        let session = self
            .manager
            .get(id)
            .ok_or_else(|| TransportError::Runtime(format!("terminal not found: {}", id)))?;
        session.resize(cols, rows).map_err(TransportError::Runtime)
    }

    fn close(&self, handle: &TransportHandle) -> Result<(), TransportError> {
        let id = match self.handle_terminal_id(handle) {
            Ok(id) => id,
            Err(e) => {
                // Idempotent close — wrong handle is logged, not propagated.
                warn!("PTY close on wrong handle: {}", e);
                return Ok(());
            }
        };
        match self.manager.close(id) {
            Ok(()) => Ok(()),
            Err(e) if e.contains("not found") => Ok(()), // idempotent
            Err(e) => Err(TransportError::Runtime(e)),
        }
    }

    /// Phase 8 (plan §D10) — tap the terminal's output broadcast for opt-in
    /// streaming. Looks up the `TerminalSession` by id and subscribes to its
    /// output channel (base64 chunks). Returns `None` if the terminal isn't
    /// found (already closed) or the handle is the wrong kind.
    fn tap_output(
        &self,
        handle: &TransportHandle,
    ) -> Option<tokio::sync::broadcast::Receiver<String>> {
        let id = self.handle_terminal_id(handle).ok()?;
        let session = self.manager.get(id)?;
        Some(session.subscribe_output())
    }
}
