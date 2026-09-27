//! Out-of-process PTY holder: one process per pane that owns the PTY master,
//! so a terminal-hosted session survives the runner exiting.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`.
//! D3: one binary, two modes — the holder is the runner binary re-exec'd with
//! a pre-GUI argv flag, dispatched from `main()` before any Tauri or
//! single-instance init. D13: one holder per pane, not one daemon.
//!
//! **Phase 0 (this module today) is the go/no-go spike only** — [`spike`]
//! opens a PTY, spawns a child, reports the pids and serves nothing. It exists
//! to answer one question with a test: does a held PTY's child outlive the
//! process that spawned the holder, with the same pid? Phase 1 grows it into
//! the real `--pty-holder` (lock file, local IPC, answered handshake).

pub mod spike;

pub use spike::try_run_spike;
