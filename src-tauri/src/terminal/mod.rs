//! Embedded terminal system — PTY-backed terminal sessions.
//!
//! Provides full terminal emulation inside the runner via `portable-pty`.
//! Each session spawns a native shell (PowerShell on Windows, $SHELL on Unix)
//! with proper environment for running Claude CLI and other dev tools.

pub mod grid;
pub mod interceptor;
pub mod manager;
pub mod session;
pub mod transcript;
pub mod types;

pub use manager::TerminalManager;
