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

use std::process::{Command, Stdio};

/// Pick the preferred Node package manager. Order: `pnpm`, then `npm`.
/// Resolved at startup and at each install (cheap — `--version` is fast).
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

/// Probe a package-manager binary by running `<name> --version`, swallowing
/// all output. Returns true iff it exits 0.
pub fn probe_pm(name: &str) -> bool {
    let mut cmd = Command::new(name);
    cmd.arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
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
