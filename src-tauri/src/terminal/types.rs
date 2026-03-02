//! Type definitions for the embedded terminal system.

use serde::{Deserialize, Serialize};

/// Unique identifier for a terminal session.
pub type TerminalId = String;

/// Information about a terminal session, returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub title: String,
    pub pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub working_dir: String,
    pub is_alive: bool,
    pub exit_code: Option<i32>,
}

/// Event payload emitted when a terminal produces output.
#[derive(Debug, Clone, Serialize)]
pub struct TerminalOutputEvent {
    pub terminal_id: TerminalId,
    /// Base64-encoded bytes (xterm.js needs raw bytes, not UTF-8 strings,
    /// because PTY output can contain partial UTF-8 sequences).
    pub data: String,
}

/// Event payload emitted when a terminal process exits.
#[derive(Debug, Clone, Serialize)]
pub struct TerminalExitEvent {
    pub terminal_id: TerminalId,
    pub exit_code: Option<i32>,
}
