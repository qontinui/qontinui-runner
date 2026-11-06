//! The one writer for every test shim this binary EXECS.
//!
//! [`write_executable_script`] writes `body` to `path` as a mode-755 script
//! WITHOUT this process ever holding a write descriptor on it, so an exec of
//! it can never fail ETXTBSY. Every script a test in this crate execs — a fake
//! binary, a PATH shim, a git hook — is written through here.
//!
//! Why a plain `fs::write` + `set_permissions` flakes (CI job 123922633970,
//! `fleet::tests::cred_absent_aws_retracts_on_next_heartbeat_payload`): while
//! the writing thread's fd is open, a sibling test thread that forks copies it
//! into its child, where it stays open until that child execs (`O_CLOEXEC`
//! closes it only then). The file's write count is held for that window, and
//! an exec of the script inside it is refused ETXTBSY.
//!
//! Two things that look like fixes are not:
//!
//! - A lock. The forking thread can live in ANY module of the test binary, so
//!   a lock one module's tests share serialises nothing that matters.
//! - Write-to-temp + `rename`. The leaked descriptor pins the INODE, and
//!   `rename` moves that same inode into place.
//!
//! Handing the write to a child `sh` does fix it: the descriptor then exists
//! only in that child's fd table, which no fork of this process copies, and it
//! is gone once the child is reaped — before the helper returns. It is
//! stressed by `fleet::tests::cred_shim_is_execable_while_other_threads_fork`
//! under four forking threads.

#[cfg(not(windows))]
use std::path::Path;

/// Write `body` to `path` as a mode-755 script, the write performed by a child
/// `/bin/sh` so this process never holds a write fd on it. Panics, carrying the
/// writer's stderr, when the write fails.
#[cfg(not(windows))]
pub(crate) fn write_executable_script(path: &Path, body: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut writer = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"cat >"$1" && chmod 755 "$1""#)
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn /bin/sh to write the shim");
    // A writer that died early (say, the directory is missing) closes the
    // pipe: let the exit-status assert below report its stderr instead.
    if let Err(e) = writer
        .stdin
        .take()
        .expect("the writer's stdin is piped")
        .write_all(body.as_bytes())
    {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe,
            "feed the shim body to its writer: {e}"
        );
    }
    let out = writer.wait_with_output().expect("reap the shim writer");
    assert!(
        out.status.success(),
        "writing the shim {} failed ({}): {}",
        path.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::write_executable_script;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_the_body_at_mode_755_and_it_execs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shim");
        let body = "#!/bin/sh\necho shim-ran \"$1\"\n";
        write_executable_script(&path, body);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "mode was {mode:o}");

        let out = std::process::Command::new(&path)
            .arg("arg1")
            .output()
            .expect("exec the written shim");
        assert!(out.status.success(), "shim exited {}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "shim-ran arg1\n");
    }

    #[test]
    #[should_panic(expected = "writing the shim")]
    fn a_missing_directory_panics_with_the_writers_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        write_executable_script(&tmp.path().join("absent").join("shim"), "#!/bin/sh\n");
    }
}
