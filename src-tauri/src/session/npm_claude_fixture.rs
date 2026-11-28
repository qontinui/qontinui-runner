//! A REAL npm-installed Claude Code, as far as the runner's liveness readers
//! can tell: a live process whose image is `node` and whose own
//! `CLAUDE_CONFIG_DIR` holds a `sessions/<pid>.json` record naming it (plan
//! `2026-11-18-runner-npm-claude-liveness-follow-ups-arming-through-population-entries-and-round-3-nits`,
//! Phase 1).
//!
//! The unit tests of #2967 judged hand-built tables
//! (`PaneLook::over_whole_table`); this fixture is what lets a test cross the
//! real process table and the real registry read instead — the part #2967
//! changed the readers of — through `PaneLook::take_fresh` and the doors that
//! call it (`tracking_health::compute`, `tracking_health::poll_tick_look`).
//!
//! How it is built, and why each piece:
//!
//! - **The image.** A symlink named `node` to the `sleep` binary, exec'd by
//!   its symlink path, with `argv[0]` set to `sleep`. That the process table
//!   then reads `node` is not assumed: the spawn waits for the table to show
//!   the child as a live `node` with a start time and panics, loudly, if it
//!   never does (the shell-hosted form, which cannot set `argv[0]`, gets the
//!   same check) rather than skipping.
//! - **The config dir.** `CLAUDE_CONFIG_DIR=<tmp>/cfg` is set in the CHILD's
//!   environment only. The registry witness reads each JS-runtime candidate's
//!   own `CLAUDE_CONFIG_DIR` from `/proc/<pid>/environ`
//!   (`process_capture::process_tree::config_dir_of_pid`), so the record is
//!   found per child without touching this process's environment — the
//!   plan's Phase 1 exit criterion. The spawn asserts that read answers.
//! - **The record.** The shape `graceful_exit`'s test helper `npm_record`
//!   writes, with `startedAt` one second after the process's start as the
//!   SAME process-table read reports it (`creation_times`, epoch seconds), so
//!   the `startedAt` identity check reads the same process.
//!
//! Linux only: both the `/proc` table and the environ read are Linux's; on any
//! other OS the witness cannot see the child's config dir at all.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::process_capture::process_tree::ProcessSnapshot;

/// Serialises every test that spawns a fixture. The child sits in the test
/// process's subtree, so any concurrent census rooted there counts it; tests
/// assert by the child's pid, and this lock keeps two fixture tests from
/// reading each other's children (or each other's unlistable dirs) at all.
/// A tokio mutex: it is held across `.await`, and it is not poisoned when a
/// test panics.
pub(crate) static FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long the spawn waits for the process table to show the child as
/// `node` before failing loudly.
const SPAWN_DEADLINE: Duration = Duration::from_secs(10);

/// How long the child sleeps — far longer than any test; it is killed on drop.
/// Kept short because `Drop` does not run when the test binary is SIGKILLed
/// (a harness timeout), and an orphaned `node` then lives this long.
const SLEEP_SECS: &str = "120";

/// Who is the fixture's parent.
enum Host {
    /// The `node` process itself, a direct child of the test process.
    Direct(Child),
    /// An `sh` that runs `node` as its child, in its own process group — the
    /// shape of a terminal pane (shell → `claude`).
    Shell(Child),
}

/// See the module docs.
pub(crate) struct NpmClaudeFixture {
    dir: tempfile::TempDir,
    node_pid: u32,
    /// The `node` process's start time as the process table reports it
    /// (epoch seconds).
    started_s: i64,
    host: Host,
}

impl NpmClaudeFixture {
    /// A `node` child of the test process, its record written.
    pub(crate) async fn spawn() -> Self {
        let dir = tempfile::tempdir().expect("npm-claude fixture: create a tempdir");
        let node = node_link(dir.path());
        let child = Command::new(&node)
            .arg0("sleep")
            .arg(SLEEP_SECS)
            .env("CLAUDE_CONFIG_DIR", dir.path().join("cfg"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("npm-claude fixture: spawn {}: {e}", node.display()));
        let node_pid = child.id();
        Self::finish(dir, Host::Direct(child), |_| Some(node_pid)).await
    }

    /// An `sh` child of the test process running the `node` child — the
    /// terminal-pane shape the lifecycle poll judges. [`Self::shell_pid`] is
    /// the pane root.
    pub(crate) async fn spawn_under_shell() -> Self {
        let dir = tempfile::tempdir().expect("npm-claude fixture: create a tempdir");
        let node = node_link(dir.path());
        let sh = Command::new("sh")
            .arg("-c")
            .arg(format!("\"$0\" {SLEEP_SECS} & wait"))
            .arg(&node)
            .env("CLAUDE_CONFIG_DIR", dir.path().join("cfg"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap_or_else(|e| panic!("npm-claude fixture: spawn sh: {e}"));
        let sh_pid = sh.id();
        // The `node` pid is read from the table: the one `node`-named child of
        // the shell.
        Self::finish(dir, Host::Shell(sh), move |snap: &ProcessSnapshot| {
            snap.parent_map
                .get(&sh_pid)?
                .iter()
                .copied()
                .find(|kid| snap.names.get(kid).map(String::as_str) == Some("node"))
        })
        .await
    }

    /// Wait until the process table shows the `node` child (found by
    /// `find_node`) with a start time, check every precondition the witness
    /// depends on, and write the record.
    async fn finish(
        dir: tempfile::TempDir,
        mut host: Host,
        find_node: impl Fn(&ProcessSnapshot) -> Option<u32>,
    ) -> Self {
        let deadline = tokio::time::Instant::now() + SPAWN_DEADLINE;
        let (node_pid, started_s) = loop {
            let snap = crate::process_capture::process_tree::snapshot_process_table_fresh().await;
            let found = (!snap.is_unknown())
                .then(|| find_node(&snap))
                .flatten()
                .and_then(|pid| {
                    let name = snap.names.get(&pid).map(String::as_str);
                    let started = snap.creation_times.get(&pid).copied().unwrap_or(0);
                    (name == Some("node") && started > 0).then_some((pid, started))
                });
            if let Some(found) = found {
                break found;
            }
            let child = match &mut host {
                Host::Direct(child) | Host::Shell(child) => child,
            };
            if let Ok(Some(status)) = child.try_wait() {
                host.kill_and_reap();
                panic!(
                    "npm-claude fixture: the fixture process exited ({status}) before the \
                     process table showed a `node` child — a `sleep` that cannot run under the \
                     name `node` (a multi-call binary?) breaks this fixture"
                );
            }
            if tokio::time::Instant::now() >= deadline {
                // No `Self` exists yet, so its `Drop` will not reap the host:
                // do it here, or a failed spawn leaves the `node` running.
                host.kill_and_reap();
                panic!(
                    "npm-claude fixture: the process table never showed the fixture's child as a \
                 `node` process with a start time within {SPAWN_DEADLINE:?} (table unknown: {})",
                    snap.is_unknown()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let fixture = Self {
            dir,
            node_pid,
            started_s,
            host,
        };
        // The Phase 1 exit criterion, asserted rather than assumed: the
        // registry witness can see THIS child's config dir through its own
        // environment, with nothing set in the test process's.
        let seen = crate::process_capture::process_tree::config_dir_of_pid(node_pid);
        assert_eq!(
            seen.as_deref().map(Path::new),
            Some(fixture.config_dir().as_path()),
            "npm-claude fixture: /proc/{node_pid}/environ must carry the child's own \
             CLAUDE_CONFIG_DIR — the registry witness reads it there"
        );
        fixture.write_record();
        fixture
    }

    /// The `node` process's pid.
    pub(crate) fn pid(&self) -> u32 {
        self.node_pid
    }

    /// The hosting `sh`'s pid, for [`Self::spawn_under_shell`].
    pub(crate) fn shell_pid(&self) -> Option<u32> {
        match &self.host {
            Host::Direct(_) => None,
            Host::Shell(sh) => Some(sh.id()),
        }
    }

    /// The `node` process's start time as the process table reports it
    /// (epoch seconds).
    pub(crate) fn started_s(&self) -> i64 {
        self.started_s
    }

    /// The child's `CLAUDE_CONFIG_DIR`.
    pub(crate) fn config_dir(&self) -> PathBuf {
        self.dir.path().join("cfg")
    }

    fn sessions_dir(&self) -> PathBuf {
        self.config_dir().join("sessions")
    }

    fn record_path(&self) -> PathBuf {
        self.sessions_dir().join(format!("{}.json", self.node_pid))
    }

    /// (Re)write the record naming the child: `graceful_exit`'s `npm_record`
    /// shape, `startedAt` one second after the process's start.
    pub(crate) fn write_record(&self) {
        std::fs::create_dir_all(self.sessions_dir()).expect("npm-claude fixture: sessions dir");
        let pid = self.node_pid;
        let started_at_ms = self.started_s * 1000 + 1_000;
        std::fs::write(
            self.record_path(),
            format!(
                r#"{{"pid":{pid},"sessionId":"npm-session","status":"idle","startedAt":{started_at_ms}}}"#
            ),
        )
        .expect("npm-claude fixture: write the record");
    }

    /// Remove the record, leaving `sessions/` listable: the child is then a
    /// plain `node` no record names.
    pub(crate) fn drop_record(&self) {
        std::fs::remove_file(self.record_path()).expect("npm-claude fixture: drop the record");
    }

    /// Make `sessions/` exist and be unlistable: the dir is replaced by a
    /// regular file, so listing it fails with an error that is not
    /// `NotFound` (a missing dir is an ABSENCE, which decides). A file, not a
    /// `chmod 000` dir: a test run as root lists that anyway.
    pub(crate) fn make_sessions_unlistable(&self) {
        let sessions = self.sessions_dir();
        if sessions.is_dir() {
            std::fs::remove_dir_all(&sessions).expect("npm-claude fixture: remove sessions/");
        }
        std::fs::write(&sessions, b"not a directory")
            .expect("npm-claude fixture: put a file where sessions/ was");
        assert!(
            std::fs::read_dir(&sessions).is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound),
            "npm-claude fixture: {} must exist and fail to list",
            sessions.display()
        );
    }
}

impl Host {
    /// Kill and reap the host — and, for the shell form, its whole process
    /// group, so the `node` is never orphaned past the test.
    fn kill_and_reap(&mut self) {
        match self {
            Host::Direct(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Host::Shell(sh) => {
                // The whole group — the shell and its `node` — so the `node`
                // is never orphaned past the test.
                if let Ok(pgid) = libc::pid_t::try_from(sh.id()) {
                    // SAFETY: `kill` takes no pointers; `-pgid` names the
                    // process group this fixture created (`process_group(0)`
                    // makes the shell its leader) while the shell is still an
                    // unreaped child, so the id cannot have been reused.
                    unsafe { libc::kill(-pgid, libc::SIGKILL) };
                }
                let _ = sh.kill();
                let _ = sh.wait();
            }
        }
    }
}

impl Drop for NpmClaudeFixture {
    fn drop(&mut self) {
        self.host.kill_and_reap();
    }
}

/// A symlink named `node` to the `sleep` binary, in `dir`.
fn node_link(dir: &Path) -> PathBuf {
    let sleep = ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .expect("npm-claude fixture: no `sleep` at /bin/sleep or /usr/bin/sleep");
    let node = dir.join("node");
    std::os::unix::fs::symlink(&sleep, &node).unwrap_or_else(|e| {
        panic!(
            "npm-claude fixture: symlink {} -> {}: {e}",
            node.display(),
            sleep.display()
        )
    });
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::graceful_exit::{PaneLook, SubtreeClaudes};

    /// The test process's subtree as one look takes it.
    async fn reading() -> SubtreeClaudes {
        let me = std::process::id();
        let look = PaneLook::take_fresh(&[me]).await;
        assert!(
            !look.snapshot().is_unknown(),
            "the process table could not be read"
        );
        look.claude_reading(me)
            .expect("a look rooted at the test process answers for it")
    }

    /// Falsifies the fixture before anything depends on it: through the real
    /// `PaneLook::take_fresh` — the real `/proc` table and the real registry
    /// read — the child is a live `claude` with its record, a plain `node`
    /// with the record dropped, and UNDECIDED with `sessions/` unlistable.
    #[tokio::test]
    async fn the_npm_claude_fixture_reads_live_then_absent_then_undecided() {
        let _serial = FIXTURE_LOCK.lock().await;
        let fixture = NpmClaudeFixture::spawn().await;
        let pid = fixture.pid();
        let undecided = |r: &SubtreeClaudes| r.undecided.iter().any(|(p, _)| *p == pid);

        let named = reading().await;
        assert!(
            named.live.contains(&pid),
            "a `node` its record names is a live claude: {named:?}"
        );
        assert!(!undecided(&named), "{named:?}");

        fixture.drop_record();
        let dropped = reading().await;
        assert!(
            !dropped.live.contains(&pid) && !undecided(&dropped),
            "a `node` no record names is not a claude (any other unlistable sessions dir on \
             this box would make it undecided instead): {dropped:?}"
        );

        fixture.make_sessions_unlistable();
        let unlistable = reading().await;
        assert!(
            !unlistable.live.contains(&pid) && undecided(&unlistable),
            "a `node` whose sessions dir cannot be listed is undecided, never absent: \
             {unlistable:?}"
        );
    }
}
