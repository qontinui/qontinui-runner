//! Terminal session DTOs — thin re-export wrapper over `qontinui_types::terminal`.
//!
//! The wire-format DTOs (`TerminalInfo`, `TerminalOutputEvent`,
//! `TerminalExitEvent`, and the `TerminalId` alias) live in
//! `qontinui_types::terminal`; this module re-exports them so the rest of the
//! runner can continue to `use crate::terminal::types::*` unchanged.
//!
//! Runtime-only state — PTY master/writer handles, tokio broadcast channels,
//! OS thread join handles, scrollback ring buffers, atomic flow-control
//! counters — lives on `TerminalSession` in
//! [`crate::terminal::session`]. None of that crosses the wire, so none of
//! it belongs in the shared types crate.

pub use qontinui_types::terminal::*;

/// Identity of the REMOTE session a tab mirrors (plan
/// `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 4).
///
/// Runner-local: `TerminalInfo` is a shared-schema type
/// (`qontinui_types::terminal`) and gains no field here, so the identity
/// rides beside it — in [`RemoteTerminalInfo`] from `terminal_attach_remote`,
/// in the `terminal-remote-identity` event that follows `terminal-created`,
/// and in `terminal_remote_identities` for a reconnecting webview. The tab is
/// keyed on `(device_id, session_id)`: the local terminal id is fresh per
/// attach, and a remote session that leaves and re-enters the fleet list
/// keeps its tab state under this pair.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTabIdentity {
    /// The device the session runs on (coord's device id).
    pub device_id: String,
    /// Display name for the tab title / badge — the picker row's label at
    /// attach time, or the device id's stem when the row had none.
    pub device_label: String,
    /// The coord session id on the target.
    pub session_id: String,
    /// The TARGET runner's terminal id (not a local id).
    pub remote_terminal_id: String,
    /// The grant this pane is bound under. Not serialized: the frontend has
    /// no use for a capability id, and the reattach path mints a fresh one.
    #[serde(skip_serializing)]
    pub grant_jti: String,
    /// True when the target holds ring bytes OLDER than the attach seed —
    /// the "Load earlier output" affordance is offered only then.
    pub history_available: bool,
}

/// What `terminal_attach_remote` returns: the ordinary `TerminalInfo` the
/// frontend opens as a tab, plus the remote identity (flattened so every
/// `TerminalInfo` reader keeps working on it unchanged).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalInfo {
    #[serde(flatten)]
    pub info: TerminalInfo,
    pub remote: RemoteTabIdentity,
}
