//! Out-of-process PTY holder: one process per pane that owns the PTY master,
//! so a terminal-hosted session survives the runner exiting.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`.
//! D13: one holder per pane, not one daemon. D3 as decided 2026-09-28: the
//! holder is a DEDICATED binary (`crates/pty-holder`, bin
//! `qontinui-pty-holder`), not this runner re-exec'd.
//!
//! Phase 2 as built:
//!
//! - [`spawn`] (this module's one child) — the runner's door to spawning a
//!   pane's holder: locating the bundled holder binary and the per-instance
//!   pane directory, over the holder library's spawner (cgroup escape, the
//!   typed `unprotected` outcome, Windows job breakaway, verified-pid
//!   teardown, reaping).
//! - The pane side is the bin's `terminal::daemon_pane_io::DaemonPaneIo`, the
//!   third `PaneIo` impl, chosen per spawn by the default-OFF
//!   `terminal.pty_holder` setting (`settings::TerminalSettings`).
//!
//! The Phase 0 survival spike (`--pty-holder-spike` in `main()`, its module and
//! its integration test) was deleted here: its spawn requirements now live in
//! `qontinui_pty_holder::spawn`, whose own tests drive the real holder binary
//! (`crates/pty-holder/tests/pty_holder_spawn.rs`, including survival of a
//! SIGKILLed spawner). The Windows outer-job survival matrix it carried is
//! Phase 3's; `qontinui_runner_win32::holder_spawn::OuterKillOnCloseJob` is
//! kept for it.

pub mod spawn;
