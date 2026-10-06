//! The per-pane PTY holder: frame protocol, local IPC transport, and
//! lock-before-endpoint liveness.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 1. A terminal-hosted agent session dies with the runner today because
//! the runner owns its PTY. The plan moves each pane's PTY into its own small
//! holder process (D13: one holder per pane) that outlives the runner; this
//! crate is that holder's skeleton and the runner's way of talking to it.
//!
//! **Phase 2: the holder owns its pane's PTY.** It is started with the child's
//! spec ([`spec`]), takes its lock, spawns the child on a PTY it keeps ([`pty`]),
//! binds its endpoint, and serves the data path (protocol version 2: attach
//! with an offset, output frames with absolute offsets, input, resize,
//! pause/resume, kill, detach, exit) beside Phase 1's handshake / `census` /
//! `ping` / `prepare_upgrade`. The runner spawns holders through [`spawn`];
//! its `DaemonPaneIo` is the client of [`client::Client::attach`].
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
//! - [`spec`] — the child spec, delivered as a consumed 0600 file.
//! - [`ring`] — the bounded output ring with absolute offsets.
//! - [`pty`] — the PTY, the child, and when the holder may exit.
//! - [`startup`] — fd closing, `SIGCHLD`, `setsid`, stdio: the holder's first
//!   acts.
//! - [`server`] — the holder: start-up order, the allowlisted dispatch, the
//!   per-connection output pump.
//! - [`client`] — connect + handshake, `probe`, `census`, `attach`.
//! - [`spawn`] — the RUNNER side of a holder's birth: cgroup escape, Windows
//!   job breakaway, the ready line, verified-pid teardown, reaping.

pub mod client;
pub mod frame;
pub mod lock;
pub mod pane;
pub mod protocol;
pub mod pty;
pub mod ring;
pub mod server;
pub mod spawn;
pub mod spec;
pub mod startup;
pub mod terminate;
pub mod transport;

#[cfg(test)]
mod source_guard;
