//! Transport layer for [`crate::session::Session`].
//!
//! A transport is the thing that actually runs the work — a PTY-backed
//! shell, a Claude CLI subprocess, or the workflow executor. The
//! [`Transport`] trait wraps the three so [`crate::session::Session`]
//! treats them uniformly: select by `intent.kind`, drive `start` → `close`,
//! pump input/resize while alive.
//!
//! Plan §Phase 2 (`session/transport/{pty,claude_cli,workflow}.rs`) — each
//! impl delegates to the existing module that owns the actual subprocess
//! lifecycle (`terminal::TerminalManager`, `claude_session::ClaudeSession`,
//! `unified_workflow_executor`). Phase 9 cleanup absorbs those callees;
//! during Phase 2 we keep them in place so the runner stays operable while
//! the new surface lands.

pub mod claude_cli;
pub mod pty;
pub mod workflow;

use std::sync::Arc;

use super::intent::Intent;

/// Concrete handle returned by [`Transport::start`]. Holds whatever each
/// transport needs to route subsequent input / resize / close calls back
/// to its underlying subprocess.
///
/// Variant is the only `pub` field; consumers should not pattern-match the
/// payload and instead route via the trait methods.
#[derive(Debug)]
pub enum TransportHandle {
    /// Tied to a [`crate::terminal::TerminalManager`] terminal id.
    Pty { terminal_id: String },
    /// Tied to a [`crate::claude_session::SessionManager`] CLI session id.
    ClaudeCli { cli_session_id: String },
    /// Workflow runs are managed end-to-end by the workflow executor; the
    /// transport just records the `task_run_id` for log/observation
    /// hookups.
    Workflow { task_run_id: String },
}

/// Errors raised by transports. Boxed so the variant set is open and each
/// transport can carry its native error context without exploding the
/// shared enum.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport rejected intent: {0}")]
    InvalidIntent(String),
    #[error("transport not yet implemented for kind: {0}")]
    Unsupported(&'static str),
    #[error("transport runtime error: {0}")]
    Runtime(String),
}

/// What [`crate::session::Session`] needs from each transport.
///
/// The trait is intentionally narrow: the broad surface (scrollback, grid
/// snapshots, OSC title sync, etc.) lives on each transport's native
/// manager and is reached via the [`TransportHandle`] payload.
///
/// `start` takes `&self` so a single manager-style transport (one Arc held
/// by Tauri state, see [`pty::PtyTransport`]) can spawn many sessions.
pub trait Transport: Send + Sync + 'static {
    /// Spawn the underlying subprocess / runtime for `intent` and return a
    /// [`TransportHandle`] that subsequent ops route through.
    fn start(&self, intent: &Intent) -> Result<TransportHandle, TransportError>;

    /// Write input bytes. For [`TransportHandle::Pty`] this is PTY stdin;
    /// for [`TransportHandle::ClaudeCli`] this is a stream-json user
    /// message; workflows do not accept ad-hoc input today.
    fn write_input(&self, handle: &TransportHandle, bytes: &[u8]) -> Result<(), TransportError>;

    /// Resize the underlying terminal. No-op for transports that don't
    /// own a terminal.
    fn resize(&self, handle: &TransportHandle, cols: u16, rows: u16) -> Result<(), TransportError>;

    /// Tear down the transport. Idempotent — calling `close` on an
    /// already-closed handle returns `Ok(())`.
    fn close(&self, handle: &TransportHandle) -> Result<(), TransportError>;
}

/// Convenience alias for the dyn-trait form used by the [`super::Session`]
/// lifecycle.
pub type DynTransport = Arc<dyn Transport>;
