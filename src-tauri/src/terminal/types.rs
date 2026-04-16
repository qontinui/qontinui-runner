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
