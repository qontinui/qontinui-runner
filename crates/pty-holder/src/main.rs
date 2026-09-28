//! `qontinui-pty-holder --pane-dir <abs> --pane-id <id>`
//!
//! Phase 1: a transport-only holder. It takes `<pane-dir>/<pane-id>.lock`,
//! records itself in it, binds the pane's endpoint, prints one ready line and
//! serves the handshake / `census` / `ping` until killed. No PTY.
//!
//! The pane directory is resolved by the RUNNER (`instance::scope_path`) and
//! passed whole; the holder never derives it (plan D13, vetted 2026-09-27).
//!
//! Output, exactly one line on stdout:
//! - `holder_ready pid=<n> pane_id=<id> endpoint=<path or pipe name>`, or
//! - `holder_error=<kind> <detail>` on failure.
//!
//! Exit codes: 2 usage, 3 the pane's lock is held by another live holder,
//! 1 any other start or serve failure. A healthy holder does not exit on its
//! own in this phase.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::server::{Holder, StartError};

const USAGE: &str = "usage: qontinui-pty-holder --pane-dir <absolute dir> --pane-id <id>";

fn fail(kind: &str, detail: &str, code: u8) -> ExitCode {
    println!("holder_error={kind} {detail}");
    let _ = std::io::stdout().flush();
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let mut pane_dir: Option<PathBuf> = None;
    let mut pane_id: Option<String> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--pane-dir") => pane_dir = args.next().map(PathBuf::from),
            Some("--pane-id") => pane_id = args.next().and_then(|v| v.into_string().ok()),
            Some("--help") | Some("-h") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => return fail("usage", USAGE, 2),
        }
    }
    let (Some(pane_dir), Some(pane_id)) = (pane_dir, pane_id) else {
        return fail("usage", USAGE, 2);
    };
    let pane_id = match PaneId::new(&pane_id) {
        Ok(p) => p,
        Err(e) => return fail("usage", &e.to_string(), 2),
    };

    let holder = match Holder::start(&pane_dir, &pane_id) {
        Ok(h) => h,
        Err(e @ StartError::LockHeld { .. }) => return fail("lock_held", &e.to_string(), 3),
        Err(e) => return fail("start", &e.to_string(), 1),
    };
    println!(
        "holder_ready pid={} pane_id={} endpoint={}",
        holder.info().holder_pid,
        pane_id,
        holder.endpoint(&pane_dir).display()
    );
    let _ = std::io::stdout().flush();

    match holder.serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("serve", &e.to_string(), 1),
    }
}
