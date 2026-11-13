//! Claude CLI stream-json protocol types and codec.
//!
//! This module implements the bidirectional NDJSON protocol used by Claude CLI
//! when invoked with `--input-format stream-json --output-format stream-json`.

pub mod codec;
pub mod request_id;
pub mod types;

/// The child-env switch that makes Claude Code emit
/// `system:session_state_changed` frames (`idle` / `running` /
/// `requires_action`). Absent, the CLI sends none (Phase 2 probe Q2, Claude
/// Code 2.1.285). The structured lane sets it to `1`
/// (`ClaudeSession::finalize_child_env`).
pub const SESSION_STATE_EVENTS_ENV: &str = "CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS";
