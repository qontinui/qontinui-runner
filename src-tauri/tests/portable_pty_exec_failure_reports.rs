//! Guards the vendored `portable-pty` patch (`vendor/portable-pty-0.8.1/QONTINUI-PATCH.md`).
//!
//! An executable script whose `#!` interpreter is missing passes portable-pty's
//! own pre-fork checks and fails at `exec` inside the forked child. Unpatched,
//! the child closed std's exec-error pipe first and aborted with
//! "assertion failed: output.write(&bytes).is_ok()", and `spawn_command`
//! returned `Ok`. Patched, the errno reaches the parent as `Err`.
#![cfg(unix)]

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::os::unix::fs::PermissionsExt;

fn open_pty() -> portable_pty::PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap()
}

#[test]
fn a_failed_exec_is_reported_to_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("bad-interp.sh");
    std::fs::write(&script, "#!/nonexistent/qontinui-interp\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = match open_pty().slave.spawn_command(CommandBuilder::new(&script)) {
        Ok(_) => panic!(
            "exec failure was swallowed: spawn_command returned Ok, so the child aborted \
             unreported (the portable-pty close_random_fds patch is not applied)"
        ),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        err.contains("No such file or directory"),
        "expected the exec errno (ENOENT) to reach the parent, got: {err}"
    );
}

/// The other half of `close_random_fds`' contract: an inheritable (non-CLOEXEC)
/// fd must still not reach the child program.
#[test]
fn an_inheritable_fd_is_still_closed_in_the_child() {
    let file = std::fs::File::open("/dev/null").unwrap();
    // SAFETY: dup of a valid fd; the duplicate has no FD_CLOEXEC.
    let leaked = unsafe { libc::dup(std::os::unix::io::AsRawFd::as_raw_fd(&file)) };
    assert!(leaked > 2);

    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args([
        "-c",
        &format!("[ -e /proc/$$/fd/{leaked} ] && exit 7; exit 0"),
    ]);
    let mut child = open_pty().slave.spawn_command(cmd).unwrap();
    let status = child.wait().unwrap();
    // SAFETY: closing the duplicate we made.
    unsafe { libc::close(leaked) };
    assert!(
        status.success(),
        "inheritable fd {leaked} leaked into the child: {status:?}"
    );
}
