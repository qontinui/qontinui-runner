//! The side-effect seam every upgrade step goes through: running a program
//! under a deadline, the atomic `current`-link swap, and the few reads of the
//! process's own placement (its systemd user unit, its `QONTINUI_HOME`).
//!
//! Everything that changes the box — `systemd-run`, `systemctl`, `git`, a
//! candidate binary's `--provenance` — is a [`CommandRunner::run`] call, so the
//! preflight, the executor, the guard and the probation are tested against a
//! scripted fake that records each call and never touches a real unit.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What one program run produced. `code` is `None` when the process was ended
/// by a signal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmdOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// One line for a check's `detail`: the exit code and the first line of
    /// stderr (else stdout).
    pub fn summary(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let first = text.lines().next().unwrap_or("").trim();
        match self.code {
            Some(c) => format!("exit {c}: {first}"),
            None => format!("killed by a signal: {first}"),
        }
    }
}

/// Runs a program to completion under a deadline. The ONE door for every
/// external program the upgrade verb, its guard and its probation run.
pub trait CommandRunner: Send + Sync {
    /// Run `program args…` with stdin closed; `Err` when it could not be
    /// started or did not finish within `timeout` (it is then killed).
    fn run(&self, program: &Path, args: &[String], timeout: Duration) -> Result<CmdOutput, String>;
}

/// The real [`CommandRunner`].
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, program: &Path, args: &[String], timeout: Duration) -> Result<CmdOutput, String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", program.display()))?;
        // Drain both pipes on their own threads so a chatty child can never
        // fill a pipe buffer and stall behind our wait.
        let out_pipe = child.stdout.take();
        let err_pipe = child.stderr.take();
        let drain = |pipe: Option<std::process::ChildStdout>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                String::from_utf8_lossy(&buf).into_owned()
            })
        };
        let out_thread = drain(out_pipe);
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = err_pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{} did not finish within {}s and was killed",
                        program.display(),
                        timeout.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(format!("waiting for {}: {e}", program.display())),
            }
        };
        Ok(CmdOutput {
            code: status.code(),
            stdout: out_thread.join().unwrap_or_default(),
            stderr: err_thread.join().unwrap_or_default(),
        })
    }
}

/// `systemctl` / `systemd-run`, resolved through `PATH` like the unit files
/// resolve them.
pub const SYSTEMCTL: &str = "systemctl";
pub const SYSTEMD_RUN: &str = "systemd-run";
pub const GIT: &str = "git";

/// The deadline for one `systemctl` / `git` / candidate `--…` read.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// `systemctl --user <args…>`.
pub fn systemctl_user(runner: &dyn CommandRunner, args: &[&str]) -> Result<CmdOutput, String> {
    let mut all = vec!["--user".to_string()];
    all.extend(args.iter().map(|a| (*a).to_string()));
    runner.run(Path::new(SYSTEMCTL), &all, COMMAND_TIMEOUT)
}

/// `systemctl --user show -p <prop> --value <unit>`, trimmed. `Err` when the
/// read failed — never an empty string standing in for "unset".
pub fn systemd_show(runner: &dyn CommandRunner, unit: &str, prop: &str) -> Result<String, String> {
    let out = systemctl_user(runner, &["show", "-p", prop, "--value", unit])?;
    if !out.success() {
        return Err(format!(
            "systemctl --user show {unit} -p {prop}: {}",
            out.summary()
        ));
    }
    Ok(out.stdout.trim().to_string())
}

/// The systemd USER unit this process runs in, read from `/proc/self/cgroup`
/// (`…/user@1000.service/app.slice/qontinui-runner.service`). `None` off
/// Linux, outside a user manager, or when the cgroup names no `.service`.
pub fn own_systemd_user_unit() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    systemd_user_unit_from_cgroup(&text)
}

/// Pure half of [`own_systemd_user_unit`].
pub fn systemd_user_unit_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        // cgroup v2: `0::<path>`; v1 lines name a controller instead.
        let path = line.splitn(3, ':').nth(2)?;
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let user_mgr = parts
            .iter()
            .position(|p| p.starts_with("user@") && p.ends_with(".service"))?;
        parts
            .iter()
            .skip(user_mgr + 1)
            .rev()
            .find(|p| p.ends_with(".service"))
            .map(|p| (*p).to_string())
    })
}

/// `${QONTINUI_HOME:-~/.qontinui}`.
pub fn qontinui_home() -> PathBuf {
    std::env::var_os("QONTINUI_HOME")
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".qontinui")
        })
}

/// `${QONTINUI_HOME:-~/.qontinui}/runner/upgrades` — the records and the
/// in-flight marker.
pub fn upgrades_dir() -> PathBuf {
    qontinui_home().join("runner").join("upgrades")
}

/// Do two git shas name the same commit? One may be abbreviated (the
/// runner's `gitSha` is 12 hex, a promoted dir 9, `PROVENANCE.json` 40): the
/// shorter must be a prefix of the longer and at least 7 characters long.
pub fn sha_agrees(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_ascii_lowercase(), b.trim().to_ascii_lowercase());
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= 7 && short.chars().all(|c| c.is_ascii_hexdigit()) && long.starts_with(&short)
}

/// Where `link` points, as an absolute path (a relative target is resolved
/// against the link's own directory, as the kernel does).
pub fn link_target(link: &Path) -> io::Result<PathBuf> {
    let raw = std::fs::read_link(link)?;
    Ok(if raw.is_absolute() {
        raw
    } else {
        link.parent().unwrap_or(Path::new(".")).join(raw)
    })
}

/// Point `link` at `target` ATOMICALLY: a new symlink is made beside it under
/// a temporary name and `rename(2)`d over it — `ln -sfn` to a temp link, then
/// `mv -T`. A reader (systemd's `ExecStart`, a holder's boot) sees the old
/// target or the new one, never a missing link. A target in the link's own
/// directory is linked by its bare name, which is how the promoted layout
/// spells it (`current -> 4d524e71b`).
#[cfg(unix)]
pub fn swap_link_atomically(link: &Path, target: &Path) -> io::Result<()> {
    let dir = link
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link has no parent"))?;
    let name = link
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link has no name"))?
        .to_string_lossy()
        .into_owned();
    let spelled: PathBuf = match target.parent() {
        Some(p) if same_dir(p, dir) => target
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| target.to_path_buf()),
        _ => target.to_path_buf(),
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.swap-{}-{nanos}", std::process::id()));
    std::os::unix::fs::symlink(&spelled, &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, link) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Off Unix there is no `current`-link layout to swap (plan Phase 8).
#[cfg(not(unix))]
pub fn swap_link_atomically(_link: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the current-link swap is Unix-only (Windows is plan Phase 8)",
    ))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Is `pid` a live process? `kill(pid, 0)`: success, or `EPERM` (alive, not
/// ours). Off Unix: `false` is never claimed — the caller gets `true` so it
/// waits rather than racing a live process.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(p) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 performs only the existence/permission check.
        let rc = unsafe { libc::kill(p, 0) };
        rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_is_read_from_the_user_manager_cgroup_only() {
        let nawa =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service\n";
        assert_eq!(
            systemd_user_unit_from_cgroup(nawa).as_deref(),
            Some("qontinui-runner.service")
        );
        let scratch =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/qr-scratch-9911.service";
        assert_eq!(
            systemd_user_unit_from_cgroup(scratch).as_deref(),
            Some("qr-scratch-9911.service")
        );
        // A login session (a terminal) is not a unit the runner can restart.
        let shell = "0::/user.slice/user-1000.slice/session-3.scope";
        assert_eq!(systemd_user_unit_from_cgroup(shell), None);
        // A system service is not a USER unit.
        assert_eq!(
            systemd_user_unit_from_cgroup("0::/system.slice/qontinui-runner.service"),
            None
        );
    }

    #[test]
    fn shas_agree_by_prefix_and_never_on_a_stub() {
        assert!(sha_agrees(
            "4d524e71b",
            "4d524e71bcc3c0ed4d9ef4b860c6ddd229f6696a"
        ));
        assert!(sha_agrees("4D524E71BCC3", "4d524e71b"));
        assert!(!sha_agrees("4d524e71b", "664deb13d"));
        assert!(!sha_agrees("4d5", "4d524e71b"));
        assert!(!sha_agrees("unknown", "unknown-thing"));
    }

    #[cfg(unix)]
    #[test]
    fn swap_is_atomic_and_relative_within_the_promoted_root() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("aaaaaaaaa");
        let b = root.path().join("bbbbbbbbb");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let link = root.path().join("current");
        swap_link_atomically(&link, &a).unwrap();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("aaaaaaaaa")
        );

        // A reader racing the swap sees one target or the other — never an
        // absent link (rename(2) replaces in one step).
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let link = link.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut reads = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let t = std::fs::read_link(&link).expect("the link is never missing");
                    assert!(t == Path::new("aaaaaaaaa") || t == Path::new("bbbbbbbbb"));
                    reads += 1;
                }
                reads
            })
        };
        for i in 0..400 {
            let target = if i % 2 == 0 { &b } else { &a };
            swap_link_atomically(&link, target).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
        assert_eq!(link_target(&link).unwrap(), root.path().join("aaaaaaaaa"));
        // No temp links are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".current.swap-")
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn real_runner_reports_exit_and_kills_on_timeout() {
        #[cfg(unix)]
        {
            let out = RealCommandRunner
                .run(
                    Path::new("sh"),
                    &["-c".into(), "echo hi; echo oops >&2; exit 3".into()],
                    COMMAND_TIMEOUT,
                )
                .unwrap();
            assert_eq!(out.code, Some(3));
            assert_eq!(out.stdout.trim(), "hi");
            assert_eq!(out.summary(), "exit 3: oops");
            let err = RealCommandRunner
                .run(
                    Path::new("sh"),
                    &["-c".into(), "sleep 5".into()],
                    Duration::from_millis(200),
                )
                .unwrap_err();
            assert!(err.contains("did not finish"), "{err}");
        }
    }
}
