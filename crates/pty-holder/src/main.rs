//! `qontinui-pty-holder --pane-dir <abs> --pane-id <id> [--report]`
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2: a holder that OWNS its pane's PTY. In order, it
//!
//! 1. closes every inherited fd >= 3 and resets `SIGCHLD` (Unix), before
//!    anything else opens a file;
//! 2. detaches from its spawner (`setsid()`; a process-group leader is refused
//!    with `holder_error=detach`, see `startup`);
//! 3. takes `<pane-dir>/<pane-id>.lock`, consumes the runner-written
//!    `<pane-id>.spec` (argv, cwd, the complete environment, size — never on
//!    argv, see `spec`), spawns the child on a PTY, records itself and the
//!    child in the lock, binds the pane's endpoint;
//! 4. prints ONE ready line, then points stdin/stdout/stderr at `/dev/null`;
//! 5. serves until the child has exited and its `exit` frame was delivered (or
//!    nobody collected it within the spec's linger), then exits with the
//!    child's status.
//!
//! The pane directory is resolved by the RUNNER (`instance::scope_path`) and
//! passed whole; the holder never derives it (plan D13, vetted 2026-09-27).
//!
//! Output, exactly one line on stdout:
//! - `holder_ready holder_pid=<n> child_pid=<n> pane_id=<id> endpoint=<path or pipe name>`, or
//! - `holder_error=<kind> <detail>` on failure.
//!
//! With `--report` the same line is ALSO written, atomically, to
//! `<pane-dir>/<pane-id>.report` (`startup::write_report`): the Windows WMI
//! spawn route (plan Phase 3) creates the holder with no stdout pipe, so the
//! spawner reads the file instead. A usage error, which precedes knowing the
//! pane, is reported on stdout only.
//!
//! Exit codes before the ready line: 2 usage, 3 the pane's lock is held by
//! another live holder, 4 the spec is missing or refused, 5 the child could
//! not be spawned, 1 any other start failure. After it: the child's exit code,
//! `128 + signal` for a signal death, 1 when unknown.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::pty::holder_exit_code;
use qontinui_pty_holder::server::{Holder, StartError};
use qontinui_pty_holder::startup;

const USAGE: &str =
    "usage: qontinui-pty-holder --pane-dir <absolute dir> --pane-id <id> [--report]";

/// Where the one report line goes besides stdout: `Some` under `--report`.
type ReportTo<'a> = Option<(&'a Path, &'a PaneId)>;

/// Print the report line, and write it to the report file when asked to.
fn report(line: &str, to: ReportTo<'_>) {
    println!("{line}");
    let _ = std::io::stdout().flush();
    if let Some((dir, pane)) = to {
        // Nothing to report a report failure to: the spawner times out and
        // says the holder was silent.
        let _ = startup::write_report(dir, pane, line);
    }
}

fn fail(kind: &str, detail: &str, code: u8, to: ReportTo<'_>) -> ExitCode {
    report(&format!("holder_error={kind} {detail}"), to);
    ExitCode::from(code)
}

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        startup::close_inherited_fds();
        startup::reset_sigchld();
    }

    let mut pane_dir: Option<PathBuf> = None;
    let mut pane_id: Option<String> = None;
    let mut to_file = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--pane-dir") => pane_dir = args.next().map(PathBuf::from),
            Some("--pane-id") => pane_id = args.next().and_then(|v| v.into_string().ok()),
            Some("--report") => to_file = true,
            Some("--help") | Some("-h") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => return fail("usage", USAGE, 2, None),
        }
    }
    let (Some(pane_dir), Some(pane_id)) = (pane_dir, pane_id) else {
        return fail("usage", USAGE, 2, None);
    };
    let pane_id = match PaneId::new(&pane_id) {
        Ok(p) => p,
        Err(e) => return fail("usage", &e.to_string(), 2, None),
    };
    let to: ReportTo<'_> = to_file.then_some((pane_dir.as_path(), &pane_id));

    if let Err(e) = startup::detach_from_spawner() {
        return fail("detach", &e.to_string(), 1, to);
    }

    let holder = match Holder::start(&pane_dir, &pane_id) {
        Ok(h) => h,
        Err(e @ StartError::LockHeld { .. }) => return fail("lock_held", &e.to_string(), 3, to),
        Err(e @ StartError::Spec(_)) => return fail("spec", &e.to_string(), 4, to),
        Err(e @ StartError::Child(_)) => return fail("child", &e.to_string(), 5, to),
        Err(e) => return fail("start", &e.to_string(), 1, to),
    };
    let info = holder.info();
    report(
        &format!(
            "holder_ready holder_pid={} child_pid={} pane_id={} endpoint={}",
            info.holder_pid,
            info.child_pid.unwrap_or(0),
            pane_id,
            holder.endpoint(&pane_dir).display()
        ),
        to,
    );
    startup::silence_stdio();

    let exit = holder.run();
    ExitCode::from(holder_exit_code(&exit))
}
