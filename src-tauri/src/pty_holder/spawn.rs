//! The runner's door to spawning a pane's PTY holder.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2. The spawn machinery itself — cgroup escape through
//! `systemd-run --user --scope`, the typed `unprotected: cgroup` outcome, the
//! Windows job-breakaway route, deadline-bounded report reads and `systemctl`
//! stops, verified-pid teardown, reaping — lives in the holder library
//! ([`qontinui_pty_holder::spawn`]) so the holder crate's own tests drive it
//! against the real holder binary on every CI leg; it is re-exported here.
//! What this module adds is what only the RUNNER knows:
//!
//! - where the holder executable is ([`resolve_holder_exe`]): beside the
//!   runner's own executable, where Tauri's `bundle.externalBin` puts the
//!   `qontinui-pty-holder` sidecar (and where a workspace `cargo build`
//!   leaves it in development). Resolve it ONCE, at a point the caller
//!   chooses (start-up), and pass the path to every spawn — `current_exe()`
//!   at spawn time breaks after an in-place rebuild (Phase 0 hand-off).
//! - where a runner instance's panes live ([`pane_dir_in`], [`PANE_DIR_NAME`]).
//!   The per-instance scoping (`instance::scope_path` over
//!   `ambient::runner_dir()`) is BIN-only, so the bin composes it:
//!   `pane_dir_in(&instance::scope_path(&runner_dir))`. The holder never
//!   re-derives it (plan D13, vetted 2026-09-27).
//! - the child's spec, which the bin builds ONLY from a sealed command:
//!   `terminal::pane_io::ScrubbedCommand::to_holder_spec` (plan D6).
//!
//! **Census vs. spawn.** A pane being spawned must be passed to
//! `qontinui_pty_holder::client::census(…, skip)` and never `probe`d until its
//! spawn returns: a probe that loses a race with the starting holder takes the
//! lock and the holder exits `lock_held` (Phase 1 hand-off). And drop every
//! `Client` / attached stream of a pane before respawning its holder — on
//! Windows a stale client handle keeps the old pipe instance alive and the new
//! holder's first-instance bind fails.
//!
//! Nothing here is wired into terminal creation yet: `DaemonPaneIo` and the
//! default-OFF `terminal.pty_holder` setting are the second half of Phase 2.

use std::path::{Path, PathBuf};

pub use qontinui_pty_holder::pane::PaneId;
pub use qontinui_pty_holder::spawn::{
    spawn_holder, HostFacts, ResolvedRoute, RouteRequest, SpawnError, SpawnRequest,
    SpawnedHolder, Unprotected, DEFAULT_REPORT_TIMEOUT,
};
pub use qontinui_pty_holder::spec::ChildSpec;

/// The holder binary's file stem (`.exe` added on Windows).
pub const HOLDER_BIN_NAME: &str = "qontinui-pty-holder";

/// The directory, under a runner instance's scoped data dir, that holds its
/// panes' lock files, sockets, specs and logs.
pub const PANE_DIR_NAME: &str = "pty-panes";

/// `<scoped runner dir>/pty-panes`.
pub fn pane_dir_in(scoped_runner_dir: &Path) -> PathBuf {
    scoped_runner_dir.join(PANE_DIR_NAME)
}

/// The holder executable beside `runner_exe`.
pub fn holder_exe_beside(runner_exe: &Path) -> Option<PathBuf> {
    let dir = runner_exe.parent()?;
    Some(dir.join(format!("{HOLDER_BIN_NAME}{}", std::env::consts::EXE_SUFFIX)))
}

/// The holder executable beside THIS process's executable, verified to be a
/// regular file. Call once, at start-up, and keep the path.
pub fn resolve_holder_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let holder = holder_exe_beside(&exe)
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))?;
    match std::fs::metadata(&holder) {
        Ok(m) if m.is_file() && m.len() > 0 => Ok(holder),
        Ok(_) => Err(format!(
            "{} is not a usable executable (empty, or not a file)",
            holder.display()
        )),
        Err(e) => Err(format!(
            "no PTY holder at {}: {e} (bundled as a Tauri externalBin; in development \
             build it with `cargo build -p qontinui-pty-holder`)",
            holder.display()
        )),
    }
}

/// Spawn the holder for `pane_id` running `child`, on the route the host's
/// observables choose. A thin, named entry point over
/// [`qontinui_pty_holder::spawn::spawn_holder`] for the runner's one call site.
pub fn spawn_pane_holder(
    holder_exe: &Path,
    pane_dir: &Path,
    pane_id: &PaneId,
    child: &ChildSpec,
) -> Result<SpawnedHolder, SpawnError> {
    spawn_holder(&SpawnRequest {
        holder_exe,
        pane_dir,
        pane_id,
        child,
        route: RouteRequest::Auto,
        report_timeout: DEFAULT_REPORT_TIMEOUT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_holder_runner_resolves_the_holder_beside_its_own_exe() {
        let p = holder_exe_beside(Path::new("/opt/qontinui/qontinui-runner")).unwrap();
        assert_eq!(
            p,
            Path::new("/opt/qontinui").join(format!(
                "qontinui-pty-holder{}",
                std::env::consts::EXE_SUFFIX
            ))
        );
        assert_eq!(
            pane_dir_in(Path::new("/d/runner/inst-a")),
            Path::new("/d/runner/inst-a/pty-panes")
        );
    }

    /// A missing holder is a typed, explanatory error — never a silent
    /// fallback to the runner's own binary.
    #[test]
    fn pty_holder_runner_missing_holder_is_an_error() {
        // The unit-test binary's directory holds no `qontinui-pty-holder`
        // unless a workspace build put one there; either answer must be a
        // regular file or an error naming the path.
        match resolve_holder_exe() {
            Ok(p) => assert!(p.is_file()),
            Err(e) => assert!(e.contains(HOLDER_BIN_NAME), "{e}"),
        }
    }
}
