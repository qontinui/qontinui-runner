//! Shared package-manager detection + subprocess-command construction.
//!
//! Extracted from `wrappers/install.rs` (the Node-wrapper install pipeline)
//! so the `install_effects_producer` route can reuse the SAME Windows-shim
//! handling without duplicating it. The wrapper pipeline still drives Node
//! installs through [`detect_node_package_manager`] + [`pm_command`]; the
//! producer additionally builds `cargo` / `pip` / `yarn` / `poetry` commands
//! through [`pm_command`].
//!
//! ## Windows note
//!
//! Node package managers ship as `.cmd` shim batch files (`pnpm.cmd`,
//! `npm.cmd`, `yarn.cmd`). Rust's `Command::new("pnpm")` does NOT walk
//! `PATHEXT` to resolve extensions, so the bare-name probe fails even when
//! the manager is installed and on PATH. We try the bare name first (works on
//! macOS/Linux and on Windows when invoked from a shell), then fall back to
//! `<name>.cmd` for Windows-shimmed installs. [`pm_command`] centralizes the
//! workaround so the install invocation hits the same resolution as the probe.

use std::process::Command;
use std::time::Duration;

/// Hard budget for one `<pm> --version` probe.
///
/// `--version` is a sub-100ms operation for every manager we probe, so this is
/// generous even for a cold `.cmd` shim on Windows. It exists because the probe
/// is a real child process: a corrupt shim, a network-mounted `node_modules`,
/// or a package manager that stops to ask something can otherwise block the
/// caller forever, and `detect_node_package_manager` fires 2-4 of these.
const PM_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pick the preferred Node package manager. Order: `pnpm`, then `npm`.
///
/// **Not cached.** Every call re-probes, so this is 2-4 bounded child processes
/// per call, and it BLOCKS. Callers on an async task must run it on the
/// blocking pool (`spawn_blocking_tracked`) — `wrappers::install` does — never
/// inline on a tokio worker thread.
///
/// Returns the bare manager name (`"pnpm"` / `"npm"`); [`pm_command`] resolves
/// the `.cmd` shim on Windows when the invocation needs it.
pub fn detect_node_package_manager() -> Option<&'static str> {
    ["pnpm", "npm"].into_iter().find(|&pm| {
        if probe_pm(pm) {
            return true;
        }
        #[cfg(windows)]
        {
            // Try the .cmd shim explicitly. We still report the bare
            // name to callers — `Command::new(pm)` later is the install
            // invocation and would hit the same failure mode, so we
            // centralize the workaround in `pm_command`.
            let shim = format!("{}.cmd", pm);
            if probe_pm(&shim) {
                return true;
            }
        }
        false
    })
}

/// Probe a package-manager binary by running `<name> --version`, discarding all
/// output. Returns true iff it exits 0 **inside [`PM_PROBE_TIMEOUT`]**.
///
/// Bounded, not a bare `Command::status()`. `status()` has no timeout: a shim
/// that never returns parks the calling thread permanently, and this function's
/// only production caller reaches it from an async request handler. A probe
/// that overruns is killed (with its whole tree — a `.cmd` shim runs the real
/// manager as a grandchild) and reported as "not usable here", which is the
/// same verdict a non-zero exit produces.
pub fn probe_pm(name: &str) -> bool {
    let mut cmd = Command::new(name);
    cmd.arg("--version");
    no_window(&mut cmd);
    // `run_with_timeout` pipes both streams and nulls stdin itself, so the
    // output is captured-and-dropped rather than inherited — same effect as the
    // previous `Stdio::null()`, minus the chance of blocking on a prompt.
    matches!(
        crate::process_helpers::run_with_timeout(cmd, PM_PROBE_TIMEOUT),
        Ok(crate::process_helpers::TimedOutput::Completed(o)) if o.status.success()
    )
}

/// Build a [`Command`] for the given package-manager binary that also works on
/// Windows where the manager is shipped as a `.cmd` shim. Applies
/// [`no_window`] so a spawned subprocess never flashes a console window on
/// Windows.
pub fn pm_command(pm: &str) -> Command {
    #[cfg(windows)]
    {
        // Try `<pm>.cmd` first; if it doesn't probe-clean, the bare name is
        // the right thing to invoke (probably resolved via PowerShell's
        // PATHEXT or a real .exe — e.g. `cargo.exe`).
        let shim = format!("{}.cmd", pm);
        if probe_pm(&shim) {
            let mut cmd = Command::new(shim);
            no_window(&mut cmd);
            return cmd;
        }
    }
    let mut cmd = Command::new(pm);
    no_window(&mut cmd);
    cmd
}

/// Apply `CREATE_NO_WINDOW` on Windows so a spawned subprocess never flashes a
/// console window. No-op on other platforms. Mirrors the in-repo precedents in
/// `process_capture::manager` and `orchestration_loop::fix_agent`.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_command_builds_for_arbitrary_binary() {
        // We can't assert the binary exists on every CI box, but constructing
        // the command must never panic and must target the requested program
        // (or its `.cmd` shim on Windows).
        let cmd = pm_command("cargo");
        let prog = cmd.get_program().to_string_lossy().to_string();
        assert!(
            prog == "cargo" || prog == "cargo.cmd",
            "unexpected program: {prog}"
        );
    }

    #[test]
    fn detect_node_pm_does_not_panic() {
        // Exercises the probe path; the result depends on the host toolchain,
        // so we only assert it returns without panicking.
        let _ = detect_node_package_manager();
    }
}
