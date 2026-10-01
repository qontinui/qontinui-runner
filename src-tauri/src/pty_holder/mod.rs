//! Out-of-process PTY holder: one process per pane that owns the PTY master,
//! so a terminal-hosted session survives the runner exiting.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`.
//! D13: one holder per pane, not one daemon. D3 as decided 2026-09-28: the
//! holder is a DEDICATED binary (`crates/pty-holder`, bin
//! `qontinui-pty-holder`), not this runner re-exec'd.
//!
//! - [`spawn`] (Phase 2) — the runner's door to spawning a pane's holder:
//!   locating the bundled holder binary and the per-instance pane directory,
//!   over the holder library's spawner.
//! - [`spike`] (Phase 0) — the go/no-go survival spike: `--pty-holder-spike`
//!   opens a PTY in the runner binary itself. Kept only until the second half
//!   of Phase 2 deletes it; its shared helpers now live in the holder library
//!   and it calls them from there.

pub mod spawn;
pub mod spike;

pub use spike::try_run_spike;
