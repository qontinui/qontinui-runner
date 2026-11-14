//! The PATH every hosted-session child starts from — one definition for the
//! PTY plane and the headless plane.
//!
//! Plan `2026-11-12-runner-hosted-linux-session-has-no-cargo-on-path`, Phase 1.
//! On a Linux box the runner is a systemd user service, and a child it spawns
//! inherits the unit's PATH, which on the measured box carried no
//! `~/.cargo/bin` and no `~/.local/bin`. A bash shell pane recovered them by
//! sourcing `~/.bashrc`; a direct-exec pane (the shape every gate continuation
//! uses) and a headless `claude -p` spawn did not, so `cargo-guard.sh` queued
//! for its build lock and then exited 127.
//!
//! The fix lives at the composer's INPUT rather than inside a spawn path:
//!
//! - [`compose_child_path`] — strips foreign shim dirs and prepends this
//!   runner's own seam dirs onto a base. Unchanged from its old home in
//!   `session.rs`.
//! - [`child_base_path`] — that base: the runner's own PATH, with each user
//!   toolchain dir not already on it appended AFTER the inherited entries, so
//!   an inherited entry keeps its order and still wins a lookup.
//!
//! The toolchain dirs come from two sources, unioned in this order:
//!
//! 1. One bounded, non-interactive login-shell read of `$SHELL` (Unix only),
//!    started at runner start off the spawn path ([`start_login_shell_read`])
//!    and cached for the process. A spawn never waits for it.
//! 2. `${CARGO_HOME:-$HOME/.cargo}/bin` and `$HOME/.local/bin`, where they
//!    exist. These are what a child gets before the read answers, or when it
//!    fails or times out — and they are kept even after it answers, because a
//!    login shell whose profile sources `~/.cargo/env` only from an
//!    interactive-guarded `~/.bashrc` answers WITHOUT the cargo dir, and the
//!    plan's arming clause is "at least `${CARGO_HOME:-$HOME/.cargo}/bin` when
//!    it exists".
//!
//! Only absolute, existing directories are appended, and never a qontinui shim
//! dir. A runner with no PATH at all gets `None` back, so a caller leaves the
//! child's PATH untouched exactly as before.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use crate::install_effects_producer::intercept::shim_materializer::is_qontinui_shim_dir;

/// Set to `0` to disable the login-shell read: it is never started, and a
/// result already cached is ignored. Any other value, or absence, leaves it on.
/// The cargo-home fallback is unaffected.
pub(crate) const SHELL_READ_ENV: &str = "QONTINUI_CHILD_PATH_SHELL_READ";

/// Hard bound on the login-shell read. On expiry the shell's whole process
/// group is killed ([`crate::process_helpers::run_with_timeout`]).
const LOGIN_SHELL_READ_TIMEOUT: Duration = Duration::from_secs(10);

const BEGIN_SENTINEL: &str = "__QONTINUI_CHILD_PATH_BEGIN__";
const END_SENTINEL: &str = "__QONTINUI_CHILD_PATH_END__";

/// The `-c` script: `$PATH` between sentinels, so whatever a profile prints
/// (a motd, a version banner) is ignored rather than parsed. `printenv`
/// rather than `"$PATH"`, because fish joins a quoted list with spaces; the
/// exported variable is `:`-joined in sh, bash, zsh and fish alike.
fn login_shell_script() -> String {
    format!("echo {BEGIN_SENTINEL} && printenv PATH && echo {END_SENTINEL}")
}

/// The login-shell read's result for this process: unset while pending,
/// `Some(dirs)` once it answered, `None` when it failed or timed out.
static LOGIN_SHELL_DIRS: OnceLock<Option<Vec<PathBuf>>> = OnceLock::new();

/// Set once [`start_login_shell_read`] has run in this process, so "never
/// started" is not reported as "pending".
static READ_STARTED: AtomicBool = AtomicBool::new(false);

/// Where the login-shell read stands — named in the config report so a reader
/// knows which source the appended dirs came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginShellRead {
    /// [`SHELL_READ_ENV`] is `0`, or this platform has no read.
    Disabled,
    /// [`start_login_shell_read`] never ran in this process (it runs from
    /// `main`), so no answer is coming.
    NotStarted,
    /// Started and not answered yet.
    Pending,
    /// Answered; its dirs are part of the appended set.
    Answered,
    /// Failed, timed out, or printed no sentinel-delimited PATH.
    Failed,
}

fn shell_read_enabled() -> bool {
    cfg!(unix) && std::env::var(SHELL_READ_ENV).ok().as_deref() != Some("0")
}

/// The current state of the login-shell read.
pub(crate) fn login_shell_read_state() -> LoginShellRead {
    if !shell_read_enabled() {
        return LoginShellRead::Disabled;
    }
    match LOGIN_SHELL_DIRS.get() {
        None if !READ_STARTED.load(Ordering::Acquire) => LoginShellRead::NotStarted,
        None => LoginShellRead::Pending,
        Some(Some(_)) => LoginShellRead::Answered,
        Some(None) => LoginShellRead::Failed,
    }
}

/// Start the login-shell read on its own thread and return at once. Called
/// once from `main`; a second call is a no-op once a result is cached.
///
/// Does nothing when [`SHELL_READ_ENV`] is `0`, on a non-Unix platform, or when
/// `$SHELL` is unset or not absolute — the fallback dirs then stand alone.
pub(crate) fn start_login_shell_read() {
    if !shell_read_enabled() || READ_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(shell) = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|s| s.is_absolute())
    else {
        let _ = LOGIN_SHELL_DIRS.set(None);
        tracing::info!("child PATH: $SHELL unset or relative; no login-shell read");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("child-path-shell-read".to_string())
        .spawn(move || {
            let dirs =
                read_login_shell_dirs(&shell, &login_shell_script(), LOGIN_SHELL_READ_TIMEOUT);
            match &dirs {
                Some(d) => tracing::info!(
                    shell = %shell.display(),
                    dirs = ?d,
                    "child PATH: login-shell read answered"
                ),
                None => tracing::warn!(
                    shell = %shell.display(),
                    "child PATH: login-shell read failed or timed out; \
                     hosted sessions get the cargo-home fallback dirs only"
                ),
            }
            let _ = LOGIN_SHELL_DIRS.set(dirs);
        });
    if let Err(e) = spawned {
        tracing::warn!("child PATH: could not start the login-shell read thread ({e})");
        let _ = LOGIN_SHELL_DIRS.set(None);
    }
}

/// Run `shell -l -c script` with stdin null and a hard timeout that kills the
/// process group, and parse the sentinel-delimited PATH it prints.
///
/// `None` on a spawn failure, a timeout, a non-zero exit, or output with no
/// sentinel pair. `script` is a parameter so a test can stand in a shell body
/// that hangs, fails or prints garbage; production passes
/// [`login_shell_script`].
#[cfg(unix)]
fn read_login_shell_dirs(shell: &Path, script: &str, timeout: Duration) -> Option<Vec<PathBuf>> {
    use crate::process_helpers::{run_with_timeout, TimedOutput};

    let mut cmd = crate::process_helpers::no_window(shell);
    cmd.arg("-l").arg("-c").arg(script);
    // The user's profile runs here: strip the credential values every other
    // child seam strips (PATH, HOME and SHELL are untouched).
    crate::terminal::scrub_credential_env_std(&mut cmd);
    // `run_with_timeout` nulls stdin, pipes both streams, arms a process group
    // before the spawn and kills it on expiry.
    match run_with_timeout(cmd, timeout) {
        Ok(TimedOutput::Completed(out)) if out.status.success() => {
            parse_sentinel_path(&String::from_utf8_lossy(&out.stdout))
        }
        _ => None,
    }
}

#[cfg(not(unix))]
fn read_login_shell_dirs(_shell: &Path, _script: &str, _timeout: Duration) -> Option<Vec<PathBuf>> {
    None
}

/// The dirs between the first sentinel pair, filtered to absolute, existing,
/// non-shim directories. `None` when no sentinel pair is present.
fn parse_sentinel_path(stdout: &str) -> Option<Vec<PathBuf>> {
    let (_, after_begin) = stdout.split_once(BEGIN_SENTINEL)?;
    let (path, _) = after_begin.split_once(END_SENTINEL)?;
    // An empty pair is a read that printed no PATH, not an answer.
    if path.trim().is_empty() {
        return None;
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    Some(keep_usable(path.trim().split(sep).map(PathBuf::from)))
}

/// Absolute, existing, non-shim directories, first spelling kept.
fn keep_usable(dirs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if d.as_os_str().is_empty()
            || !d.is_absolute()
            || !d.is_dir()
            || is_qontinui_shim_dir(&d)
            || out
                .iter()
                .any(|o| same_entry(&o.to_string_lossy(), &d.to_string_lossy()))
        {
            continue;
        }
        out.push(d);
    }
    out
}

/// `${CARGO_HOME:-$HOME/.cargo}/bin` and `$HOME/.local/bin`, where they exist.
///
/// The rule is the one `cargo-verify.sh` names in its remedy line: a non-empty
/// `CARGO_HOME` decides the cargo dir; otherwise `HOME` does. With neither set
/// nothing is returned (and nothing panics).
fn fallback_dirs(home: Option<&OsStr>, cargo_home: Option<&OsStr>) -> Vec<PathBuf> {
    let home = home.filter(|h| !h.is_empty()).map(Path::new);
    let cargo_bin = match cargo_home.filter(|c| !c.is_empty()) {
        Some(c) => Some(Path::new(c).join("bin")),
        None => home.map(|h| h.join(".cargo").join("bin")),
    };
    let local_bin = home.map(|h| h.join(".local").join("bin"));
    keep_usable(cargo_bin.into_iter().chain(local_bin))
}

/// The toolchain dirs to append: the login-shell read's (when enabled and
/// answered), then the fallback dirs. `shell_dirs` is `None` while the read is
/// pending, disabled or failed.
fn toolchain_dirs_from(
    shell_dirs: Option<&[PathBuf]>,
    home: Option<&OsStr>,
    cargo_home: Option<&OsStr>,
) -> Vec<PathBuf> {
    keep_usable(
        shell_dirs
            .unwrap_or(&[])
            .iter()
            .cloned()
            .chain(fallback_dirs(home, cargo_home)),
    )
}

fn toolchain_dirs() -> Vec<PathBuf> {
    let shell_dirs = if shell_read_enabled() {
        LOGIN_SHELL_DIRS.get().and_then(|d| d.as_deref())
    } else {
        None
    };
    // Windows normally sets no HOME; the user's home is USERPROFILE there.
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        });
    toolchain_dirs_from(
        shell_dirs,
        home.as_deref(),
        std::env::var_os("CARGO_HOME").as_deref(),
    )
}

/// The same entry comparison [`compose_child_path`] uses: trailing `/` and `\`
/// trimmed, ASCII case folded on Windows.
fn same_entry(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches(['/', '\\']);
    let b = b.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// `runner_path` with each of `toolchain` not already on it appended, in
/// order. `None` when `runner_path` is `None`.
fn base_path_from(runner_path: Option<&str>, toolchain: &[PathBuf]) -> Option<String> {
    let runner_path = runner_path?;
    let sep = if cfg!(windows) { ';' } else { ':' };
    let mut out = runner_path.to_string();
    for dir in toolchain {
        let dir = dir.to_string_lossy();
        let present = out
            .split(sep)
            .any(|entry| !entry.is_empty() && same_entry(entry, &dir));
        if present {
            continue;
        }
        if !out.is_empty() {
            out.push(sep);
        }
        out.push_str(&dir);
    }
    Some(out)
}

/// The base every hosted-session child's PATH is composed from: the runner's
/// own PATH with each user toolchain dir not already on it appended after the
/// inherited entries. `None` when the runner has no (UTF-8) PATH, in which case
/// the caller leaves the child's PATH untouched.
pub(crate) fn child_base_path() -> Option<String> {
    base_path_from(std::env::var("PATH").ok().as_deref(), &toolchain_dirs())
}

/// The toolchain dirs [`child_base_path`] appends right now — those not already
/// on the runner's PATH. Empty when the runner has no PATH. For the config
/// report's freshness line.
pub(crate) fn appended_toolchain_dirs() -> Vec<PathBuf> {
    let Ok(runner_path) = std::env::var("PATH") else {
        return Vec::new();
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    toolchain_dirs()
        .into_iter()
        .filter(|d| {
            let d = d.to_string_lossy();
            !runner_path
                .split(sep)
                .any(|entry| !entry.is_empty() && same_entry(entry, &d))
        })
        .collect()
}

/// The PATH a PTY child gets: `runner_path` with every qontinui shim dir
/// ([`is_qontinui_shim_dir`]) removed, then `own_dirs` prepended in order.
///
/// Two defects this closes (plan
/// `2026-11-04-identity-shim-foreign-shim-ping-pong-and-secondary-runner-pty-socket-overflow`,
/// Phase 2):
///
/// 1. A runner started from inside another runner's session (a supervisor
///    launched there, then a temp runner) INHERITS that session's
///    `qontinui-identity-*` dir. Passed through, it reached every pane's PATH,
///    where two identity shims could dispatch to each other for ever.
/// 2. The install-intercept seam rebuilt the child PATH from the runner's own
///    and discarded the identity seam's prepend. Both seams now build through
///    here, the install seam passing `[identity_dir, install_dir]`.
///
/// An entry equal to one of `own_dirs` is dropped from the tail as well, since
/// it is already at the head. Every other entry keeps its position and its
/// spelling, empty entries included. The separator is the platform's.
///
/// Every caller passes [`child_base_path`] as `runner_path`, so the toolchain
/// dirs it appends survive both seams.
pub(crate) fn compose_child_path(
    runner_path: Option<&str>,
    own_dirs: &[&std::path::Path],
) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let trim = |s: &str| s.trim_end_matches(['/', '\\']).to_string();
    let own: Vec<String> = own_dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    let own_trimmed: Vec<String> = own.iter().map(|d| trim(d)).collect();
    let is_own = |entry: &str| {
        let entry = trim(entry);
        own_trimmed.iter().any(|o| {
            if cfg!(windows) {
                o.eq_ignore_ascii_case(&entry)
            } else {
                *o == entry
            }
        })
    };
    let mut parts: Vec<&str> = own.iter().map(String::as_str).collect();
    if let Some(path) = runner_path.filter(|p| !p.is_empty()) {
        for entry in path.split(sep) {
            if !entry.is_empty()
                && (is_own(entry) || is_qontinui_shim_dir(std::path::Path::new(entry)))
            {
                continue;
            }
            parts.push(entry);
        }
    }
    parts.join(&sep.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdir(p: &Path) -> PathBuf {
        std::fs::create_dir_all(p).unwrap();
        p.to_path_buf()
    }

    #[test]
    fn fallback_dirs_present_and_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Absent: neither dir exists yet.
        assert!(fallback_dirs(Some(home.as_os_str()), None).is_empty());
        // Present: both.
        let cargo = mkdir(&home.join(".cargo/bin"));
        let local = mkdir(&home.join(".local/bin"));
        assert_eq!(
            fallback_dirs(Some(home.as_os_str()), None),
            vec![cargo, local]
        );
    }

    #[test]
    fn cargo_home_decides_the_cargo_dir_when_set() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let default_cargo = mkdir(&home.join(".cargo/bin"));
        let custom = tmp.path().join("custom-cargo");
        let custom_bin = mkdir(&custom.join("bin"));

        // Set: the CARGO_HOME dir, not the ~/.cargo one.
        let set = fallback_dirs(Some(home.as_os_str()), Some(custom.as_os_str()));
        assert_eq!(set, vec![custom_bin]);
        // Unset (and empty, which reads as unset): ~/.cargo/bin.
        assert_eq!(
            fallback_dirs(Some(home.as_os_str()), None),
            vec![default_cargo.clone()]
        );
        assert_eq!(
            fallback_dirs(Some(home.as_os_str()), Some(OsStr::new(""))),
            vec![default_cargo]
        );
        // A relative CARGO_HOME is not appended.
        assert!(fallback_dirs(None, Some(OsStr::new("rel/cargo"))).is_empty());
    }

    #[test]
    fn home_unset_appends_nothing_and_does_not_panic() {
        assert!(fallback_dirs(None, None).is_empty());
        assert!(fallback_dirs(Some(OsStr::new("")), None).is_empty());
        assert!(toolchain_dirs_from(None, None, None).is_empty());
        assert_eq!(
            base_path_from(Some("/usr/bin"), &toolchain_dirs_from(None, None, None)),
            Some("/usr/bin".to_string())
        );
    }

    #[test]
    fn runner_path_absent_leaves_the_child_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = mkdir(&tmp.path().join(".cargo/bin"));
        assert_eq!(base_path_from(None, &[dir]), None);
    }

    #[test]
    fn appended_after_inherited_entries_and_not_duplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let cargo = mkdir(&tmp.path().join("cargo-bin"));
        let local = mkdir(&tmp.path().join("local-bin"));
        let runner = ["/usr/bin".to_string(), "/bin".to_string()].join(sep);

        let got = base_path_from(Some(&runner), &[cargo.clone(), local.clone()]).unwrap();
        assert_eq!(
            got,
            [
                "/usr/bin".to_string(),
                "/bin".to_string(),
                cargo.to_string_lossy().into_owned(),
                local.to_string_lossy().into_owned()
            ]
            .join(sep)
        );

        // Already on PATH with a trailing separator: kept where it is, in its
        // own spelling, and not appended again.
        let trailing = format!("{}/", cargo.display());
        let runner = [trailing.clone(), "/usr/bin".to_string()].join(sep);
        let got = base_path_from(Some(&runner), &[cargo.clone(), local.clone()]).unwrap();
        assert_eq!(
            got,
            [
                trailing,
                "/usr/bin".to_string(),
                local.to_string_lossy().into_owned()
            ]
            .join(sep)
        );

        // The same dir twice in the toolchain list is appended once.
        let got = base_path_from(Some("/usr/bin"), &[cargo.clone(), cargo.clone()]).unwrap();
        assert_eq!(got.matches(&*cargo.to_string_lossy()).count(), 1);

        // An empty runner PATH yields just the toolchain dirs.
        assert_eq!(
            base_path_from(Some(""), std::slice::from_ref(&cargo)).unwrap(),
            cargo.to_string_lossy()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_variant_is_already_on_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = mkdir(&tmp.path().join("cargo-bin"));
        let upper = cargo.to_string_lossy().to_ascii_uppercase();
        let runner = format!("{upper};C:\\Windows");
        assert_eq!(base_path_from(Some(&runner), &[cargo]).unwrap(), runner);
    }

    #[test]
    fn usable_filter_keeps_only_absolute_existing_non_shim_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let good = mkdir(&tmp.path().join("good"));
        let shim = mkdir(&tmp.path().join("qontinui-shim-term-9"));
        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let printed = [
            good.to_string_lossy().into_owned(),
            "relative/bin".to_string(),
            tmp.path().join("missing").to_string_lossy().into_owned(),
            shim.to_string_lossy().into_owned(),
            file.to_string_lossy().into_owned(),
            String::new(),
            format!("{}/", good.display()),
        ]
        .join(sep);
        let out = format!("motd line\n{BEGIN_SENTINEL}{printed}{END_SENTINEL}\ntrailer");
        assert_eq!(parse_sentinel_path(&out), Some(vec![good]));
        // No sentinel pair: not an answer.
        assert_eq!(parse_sentinel_path("just garbage"), None);
        assert_eq!(
            parse_sentinel_path(&format!("{BEGIN_SENTINEL}\n{END_SENTINEL}")),
            None
        );
        assert_eq!(
            parse_sentinel_path(&format!("{BEGIN_SENTINEL}/usr/bin")),
            None
        );
    }

    #[test]
    fn a_failed_read_leaves_the_fallback_dirs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cargo = mkdir(&home.join(".cargo/bin"));
        // Failed / pending / disabled all reach here as `None`.
        assert_eq!(
            toolchain_dirs_from(None, Some(home.as_os_str()), None),
            vec![cargo.clone()]
        );
        // Answered: the shell's dirs lead, the fallback follows, no repeats.
        let shell_dir = mkdir(&tmp.path().join("pyenv-shims-not-ours"));
        assert_eq!(
            toolchain_dirs_from(
                Some(&[shell_dir.clone(), cargo.clone()]),
                Some(home.as_os_str()),
                None
            ),
            vec![shell_dir, cargo]
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_read_timeout_nonzero_exit_and_garbage_are_not_answers() {
        // The login shell sources `$HOME/.profile`: point HOME at a fixture.
        let _amb = crate::test_env::isolated_ambient();
        let sh = Path::new("/bin/sh");
        // Timeout: bounded, and the group is killed rather than waited on.
        let started = std::time::Instant::now();
        assert_eq!(
            read_login_shell_dirs(sh, "sleep 30", Duration::from_millis(300)),
            None
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the read must give up at its timeout, took {:?}",
            started.elapsed()
        );
        // Non-zero exit, even with a well-formed PATH printed first.
        let nonzero = format!("printf '%s/bin%s' '{BEGIN_SENTINEL}' '{END_SENTINEL}'; exit 3");
        assert_eq!(
            read_login_shell_dirs(sh, &nonzero, Duration::from_secs(10)),
            None
        );
        // Garbage output.
        assert_eq!(
            read_login_shell_dirs(sh, "echo garbage", Duration::from_secs(10)),
            None
        );
        // A missing shell.
        assert_eq!(
            read_login_shell_dirs(
                Path::new("/nonexistent/shell"),
                &login_shell_script(),
                Duration::from_secs(10)
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_read_with_the_production_script_answers_usable_dirs() {
        let _amb = crate::test_env::isolated_ambient();
        let got = read_login_shell_dirs(
            Path::new("/bin/sh"),
            &login_shell_script(),
            Duration::from_secs(10),
        )
        .expect("a login /bin/sh answers the production script");
        assert!(got.iter().all(|d| d.is_absolute() && d.is_dir()), "{got:?}");
    }

    #[test]
    fn shell_read_off_reports_disabled() {
        let amb = crate::test_env::isolated_ambient();
        let _env = crate::test_env::EnvVarRestore::capture(&[SHELL_READ_ENV]);
        // A test process never runs `main`, so the read was never started —
        // reported as such, not as pending.
        std::env::remove_var(SHELL_READ_ENV);
        if cfg!(unix) {
            assert_eq!(login_shell_read_state(), LoginShellRead::NotStarted);
        }
        std::env::set_var(SHELL_READ_ENV, "0");
        assert_eq!(login_shell_read_state(), LoginShellRead::Disabled);
        // And the cached result (if any) is ignored: only the fallback counts.
        let cargo = mkdir(&amb.dir().join(".cargo/bin"));
        let _cargo_home = crate::test_env::EnvVarRestore::capture(&["CARGO_HOME"]);
        std::env::remove_var("CARGO_HOME");
        assert_eq!(toolchain_dirs(), vec![cargo]);
    }
}
