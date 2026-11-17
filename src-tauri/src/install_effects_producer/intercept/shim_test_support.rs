//! The shared harness for tests that RUN a rendered bash shim template.
//!
//! One module, so there is ONE [`EXE_LOCK`], taken only through
//! [`exe_guard`]: writing an executable while another test thread forks can
//! leave the write fd open in the child and make that child's exec fail
//! `ETXTBSY`, and two locks would not serialize against each other. Its
//! holders, every test in the crate that writes an executable and then forks
//! one:
//! - `shim_materializer`'s `resolve_wait_tests`;
//! - `identity_shim_launch_failure_tests`;
//! - `identity_shim_settle_tests`;
//! - `session::spawn_prompt`'s `script_tests`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use super::shim_materializer::{render_identity_body_for_test, render_identity_for_test};

/// Names a template file to run INSTEAD of the compiled-in identity shim. The
/// template is `include_str!`'d, so without this a mutation check of it — break
/// the template, watch its test go red — costs a full rebuild of the runner's
/// test binary per mutation. Read by these tests only; unset, they run the
/// template the runner ships.
const TEMPLATE_OVERRIDE_ENV: &str = "QONTINUI_TEST_IDENTITY_SHIM_TEMPLATE";

/// The identity shim for `claude`, rendered for `shim_dir`, from the template
/// [`TEMPLATE_OVERRIDE_ENV`] names when it is set.
fn rendered_identity_shim(shim_dir: &Path) -> String {
    match std::env::var_os(TEMPLATE_OVERRIDE_ENV) {
        Some(path) => {
            let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!(
                    "{TEMPLATE_OVERRIDE_ENV}={}: {e}",
                    Path::new(&path).display()
                )
            });
            render_identity_body_for_test(&body, "claude", shim_dir)
        }
        None => render_identity_for_test("claude", shim_dir),
    }
}

/// Held by every test that writes an executable and then forks; take it only
/// through [`exe_guard`].
static EXE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`EXE_LOCK`], surviving a poisoned mutex so one failing test reports
/// its own assertion instead of cascading into the rest.
// test-lock: standalone — the crate's one exec-script lock; every holder (listed in the module doc) takes no other test lock and never takes `env_lock`
pub(crate) fn exe_guard() -> std::sync::MutexGuard<'static, ()> {
    EXE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write `body` as an executable at `path`, atomically (temp + rename) so a
/// concurrently scanning shim never sees a half-written file.
pub(crate) fn write_exe(path: &Path, body: &str) {
    let tmp = path.with_extension("partial");
    std::fs::write(&tmp, body).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// `bash` by ABSOLUTE path: these tests clear the env and set a PATH that may
/// hold no bash, and `Command` resolves a bare program name against the
/// CHILD's PATH once one is set.
pub(crate) fn bash() -> &'static str {
    ["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("/bin/bash")
}

/// Render the bash identity shim for `claude` into `shim_dir` and build
/// `bash <shim> <args>` with a CLEARED environment, `path` as `PATH` and
/// `wait` as the resolve wait. Callers add whatever other env they need.
pub(crate) fn identity_shim_cmd(shim_dir: &Path, args: &[&str], path: &str, wait: &str) -> Command {
    let shim = shim_dir.join("claude");
    write_exe(&shim, &rendered_identity_shim(shim_dir));
    // console-ok: test-only harness — this module is `#[cfg(all(test, unix))]`
    // (declared in intercept/mod.rs), so it is never built on Windows or in a release.
    let mut cmd = Command::new(bash());
    cmd.arg(&shim)
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("QONTINUI_SHIM_RESOLVE_WAIT_SECS", wait);
    cmd
}

/// `claude --version` through the identity shim with the real-tool dir, then
/// the system dirs, on PATH — the resolve-wait tests' shape.
pub(crate) fn identity_cmd(shim_dir: &Path, real_dir: &Path, wait: &str) -> Command {
    identity_shim_cmd(
        shim_dir,
        &["--version"],
        &format!(
            "{}:{}:/usr/bin:/bin",
            shim_dir.display(),
            real_dir.display()
        ),
        wait,
    )
}

/// Install a fake `stat` in `dir` that reports every target as changed at epoch
/// second 1 — long settled. The identity shim waits for a resolved `claude`
/// whose target changed under ~3 s ago (whole seconds) (plan
/// `2026-11-10-shim-settle-wait-and-in-memory-launch-liveness` Phase 3), and a
/// change time cannot be backdated (`touch -d` moves only mtime), so a test
/// that launches a freshly written fake `claude` with the real `stat` on PATH
/// would pay that wait and see its line on stderr. Put `dir` on PATH ahead of
/// the system dirs. Call it under the caller's executable-writing lock.
pub(crate) fn install_settled_stat(dir: &Path) {
    write_exe(&dir.join("stat"), "#!/bin/sh\necho 1\n");
}
