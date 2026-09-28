//! The per-pane PTY holder: frame protocol, local IPC transport, and
//! lock-before-endpoint liveness.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 1. A terminal-hosted agent session dies with the runner today because
//! the runner owns its PTY. The plan moves each pane's PTY into its own small
//! holder process (D13: one holder per pane) that outlives the runner; this
//! crate is that holder's skeleton and the runner's way of talking to it.
//!
//! **Phase 1 is transport only — there is no PTY here.** The holder takes its
//! lock, binds its endpoint, and answers the handshake, `census`, `ping` and
//! (typed `unsupported`) `prepare_upgrade`. Phase 2 adds the PTY and
//! `DaemonPaneIo` in the runner.
//!
//! Why a separate crate and binary (the D3 decision of 2026-09-28): the
//! re-exec'd runner binary cost 24.4 MB of private memory per holder, and D13
//! multiplies that by the pane count. D3's intent — the GUI and the holder
//! never drift — is kept by this LIBRARY, which both the runner (client side,
//! [`client`]) and the `qontinui-pty-holder` binary (server side, [`server`])
//! link.
//!
//! Layers:
//! - [`frame`] — `u32` BE length, 1-byte kind, payload; control frames carry
//!   JSON, data frames carry raw bytes.
//! - [`protocol`] — the frozen envelope (D15) and version negotiation.
//! - [`pane`] — pane ids and the paths / pipe name derived from them.
//! - [`lock`] — the per-pane advisory lock and its record.
//! - [`transport`] — Unix socket / Windows named pipe, OS-level authorization (D5).
//! - [`server`] — the holder: start-up order and the allowlisted dispatch.
//! - [`client`] — connect + handshake, `probe`, `census`.

pub mod client;
pub mod frame;
pub mod lock;
pub mod pane;
pub mod protocol;
pub mod server;
pub mod transport;

#[cfg(test)]
mod source_guard;
