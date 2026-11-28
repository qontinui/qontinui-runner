use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde_json::json;
use tracing::{debug, info, warn};

/// coord push tokens expire after 15 minutes (coord `PUSH_TOKEN_TTL_SECS`).
/// Refresh every 10 so a push that starts just before a tick still has a
/// 5-minute-fresh token.
const REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Consecutive failed refresh ticks after which the failure is treated as
/// terminal (session gone on coord / device auth revoked) and the loop exits.
const MAX_CONSECUTIVE_REFRESH_FAILURES: u32 = 3;

/// Config files older than this at boot are orphans (crash/kill skipped
/// cleanup) — younger files may belong to a live session, possibly of
/// another runner instance sharing the same %TEMP%.
const SWEEP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The variable names [`set_git_credential_helper`] writes under the coord
/// URL scope (`credential.<coord-scope>.<var>`) — and therefore exactly the
/// variables [`unset_orphaned_install`] removes. Kept as one list so the
/// install and teardown sides can never drift: a key added to the install
/// without a matching unset would be left behind in the worktree's
/// `config.worktree` forever (which is what happened to `useHttpPath` when it
/// was still written unscoped).
const HELPER_VAR: &str = "helper";
const USE_HTTP_PATH_VAR: &str = "useHttpPath";

/// `credential.<scope>.<var>`, the key git matches against a request URL.
fn scoped_credential_key(scope: &str, var: &str) -> String {
    format!("credential.{scope}.{var}")
}

/// Per-session credential-helper state, keyed by the stringified coord
/// session UUID (the exact `session_id` passed to
/// [`setup_credential_helper`]).
#[derive(Default)]
struct SessionCredState {
    /// Working dirs where the per-worktree helper keys were
    /// written. Drained by [`cleanup_credential_helper`], which unsets them.
    dirs: Vec<PathBuf>,
    /// Whether a token refresh loop is currently live for this session —
    /// the dedupe bit that keeps a second `setup_credential_helper` call
    /// for the SAME session (another working dir) from spawning a second
    /// loop.
    refresh_running: bool,
}

/// In-process registry of live credential-helper installs.
fn registry() -> &'static Mutex<HashMap<String, SessionCredState>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, SessionCredState>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The file stem of the helper binary; how a helper value is recognised as
/// one of ours.
const HELPER_BINARY_STEM: &str = "qontinui-git-credential";

fn credential_helper_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "qontinui-git-credential.exe"
    } else {
        "qontinui-git-credential"
    };
    let path = dir.join(name);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// The `%TEMP%` token-config path for a credential-helper install key.
/// `pub(crate)` so the teardown funnels' tests can assert the file this
/// module owns is actually gone.
pub(crate) fn config_file_path(session_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("qontinui-git-cred-{session_id}.json"))
}

async fn fetch_push_token(coord_base: &str, session_id: &str) -> Result<String, String> {
    let url = format!(
        "{}/coord/sessions/{}/push-token",
        coord_base.trim_end_matches('/'),
        session_id
    );

    let device_token = qontinui_runner_lib::auth::AuthManager::new()
        .get_access_token()
        .map_err(|e| format!("get device token: {e}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    // coord-auth-exempt(device-jwt-required): fails CLOSED — the push-token mint
    // is refused outright when no device-JWT is held, because an anonymous
    // push-token request cannot be attributed to a device. `attach_device_auth`
    // is fail-SOFT by contract, so routing this through it would convert
    // 'refuse until paired' into 'ask anonymously'.
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {device_token}"))
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "POST /coord/sessions/{session_id}/push-token returned {status}: {body}"
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse push-token response: {e}"))?;

    body.get("token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "push-token response missing 'token' field".to_string())
}

async fn fetch_registered_repos(coord_base: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/coord/canonical-repos", coord_base.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = crate::coord_http::coord_get(&client, &url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;

    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        if code == 401 || code == 403 {
            // Credential-helper setup can fire BEFORE the device-JWT exists
            // (early in session setup / before pairing completes). Once coord
            // gates canonical-repos with FleetPrincipal, the anonymous GET is
            // rejected until the token lands. Not fatal — the caller skips
            // helper install this time and retries on the next session setup.
            warn!(
                "credential_helper: canonical-repos GET unauthorized ({code}) — retrying after device pairing/auth"
            );
        }
        return Err(format!("GET /coord/canonical-repos returned {code}"));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse canonical-repos body: {e}"))?;

    Ok(body
        .get("canonical_repos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("repo").and_then(|r| r.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// Budget for every `git` call in this module.
///
/// All of them are local `rev-parse` / `config --local` writes, i.e.
/// milliseconds when healthy; the hang class is an `index.lock`. Not periodic,
/// but BURST-prone: the whole `setup_credential_helper` path runs once per
/// `terminal_create`, and ~130 concurrent session
/// spawns were observed during the 2026-08-30 wedge — 130 unbounded
/// `.output()` calls behind one wedged git is 130 lost blocking-pool threads.
const GIT_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Whether `working_dir` is a git work tree — tri-state.
///
/// The two-state `bool` this replaced folded "git answered: not a repo" into
/// the same `false` as "git never answered". [`setup_credential_helper`] skips
/// the whole install on `false`, so a `rev-parse --git-dir` killed at
/// [`GIT_CONFIG_TIMEOUT`] silently produced a session with NO credential
/// helper — i.e. the interactive `git push` password prompts this subsystem
/// exists to eliminate, in the exact situation (a contended `index.lock`) that
/// the bounded-subprocess work is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitRepoProbe {
    /// `rev-parse --git-dir` exited 0.
    Yes,
    /// git ran and said this is not a work tree.
    No,
    /// The probe was killed at its budget or could not be spawned.
    Unknown,
}

fn is_git_repo(working_dir: &Path) -> GitRepoProbe {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args([
        "-C",
        &working_dir.to_string_lossy(),
        "rev-parse",
        "--git-dir",
    ]);
    match crate::process_helpers::run_probe(
        cmd,
        GIT_CONFIG_TIMEOUT,
        "credential_helper: git rev-parse --git-dir",
    ) {
        crate::process_helpers::ProbeOutcome::Captured(_) => GitRepoProbe::Yes,
        crate::process_helpers::ProbeOutcome::Degraded(
            crate::process_helpers::DegradeReason::Status,
        ) => GitRepoProbe::No,
        crate::process_helpers::ProbeOutcome::Degraded(_) => GitRepoProbe::Unknown,
    }
}

/// Run `git -C <dir> <args>` under [`GIT_CONFIG_TIMEOUT`].
fn git_in(dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("-C").arg(dir).args(args);
    crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
        .map_err(|e| format!("run git {}: {e}", args.join(" ")))
}

/// `git -C <dir> config <scope> [--type=bool] --get <key>`: `Ok(None)` when the
/// key is absent (exit 1), the trimmed value otherwise.
fn git_config_get_in(
    dir: &Path,
    scope: &str,
    key: &str,
    as_bool: bool,
) -> Result<Option<String>, String> {
    let mut args = vec!["config", scope];
    if as_bool {
        args.push("--type=bool");
    }
    args.extend(["--get", key]);
    let out = git_in(dir, &args)?;
    if out.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ));
    }
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    Err(format!(
        "git config {scope} --get {key} failed ({:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// `git -C <dir> config --worktree <args>`, failing on a non-zero exit.
fn git_config_worktree(dir: &Path, args: &[&str]) -> Result<(), String> {
    let mut full = vec!["config", "--worktree"];
    full.extend_from_slice(args);
    let out = git_in(dir, &full)?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "git config --worktree {} failed ({:?}): {}",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Make `dir`'s repository read a per-worktree `config.worktree`: ensure
/// `extensions.worktreeConfig=true` in the shared config (read first, so the
/// steady path writes nothing).
///
/// Refuses when the shared config sets `core.bare=true` or `core.worktree`:
/// git documents that with `extensions.worktreeConfig` on, those two keys must
/// live in the main worktree's `config.worktree` instead, or every linked
/// worktree inherits them. Moving an operator's repository layout is not this
/// module's call, so the install is skipped there.
fn ensure_worktree_config_enabled(dir: &Path) -> Result<(), String> {
    if git_config_get_in(dir, "--local", "core.bare", true)?.as_deref() == Some("true") {
        return Err(
            "the shared git config sets core.bare=true; enabling extensions.worktreeConfig \
             there would hand that setting to every worktree"
                .to_string(),
        );
    }
    if let Some(value) = git_config_get_in(dir, "--local", "core.worktree", false)? {
        return Err(format!(
            "the shared git config sets core.worktree={value}; enabling \
             extensions.worktreeConfig there would hand that setting to every worktree"
        ));
    }
    let before = git_config_get_in(dir, "--local", "extensions.worktreeConfig", true)?;
    if before.as_deref() == Some("true") {
        return Ok(());
    }
    write_worktree_config_extension(dir)?;
    info!(
        "credential_helper: set extensions.worktreeConfig=true in the shared config of {} \
         (was {})",
        dir.display(),
        before.as_deref().unwrap_or("unset")
    );
    Ok(())
}

/// Write `extensions.worktreeConfig=true` to the SHARED config, tolerating a
/// concurrent first install in a sibling worktree. Nothing in this process
/// serialises writes to the shared file across worktrees, and `git config`
/// fails at once on a held `config.lock`; so a failed write re-reads the key
/// and counts `true` as success (the sibling wrote it), and otherwise retries
/// the write once after a short pause.
fn write_worktree_config_extension(dir: &Path) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        let out = git_in(
            dir,
            &["config", "--local", "extensions.worktreeConfig", "true"],
        )?;
        if out.status.success() {
            return Ok(());
        }
        last_error = String::from_utf8_lossy(&out.stderr).to_string();
        if git_config_get_in(dir, "--local", "extensions.worktreeConfig", true)
            .ok()
            .flatten()
            .as_deref()
            == Some("true")
        {
            return Ok(());
        }
    }
    Err(format!(
        "git config --local extensions.worktreeConfig true failed: {last_error}"
    ))
}

/// The lock serialising the read-modify-write of ONE worktree's helper list
/// in this process, keyed by that worktree's `config.worktree` path. Two
/// installs can land in one worktree (an agent-id install and a terminal
/// session-id install), and two interleaved "reset missing → add reset → add
/// ours" sequences would put the second empty reset AFTER the first helper,
/// which silently drops that helper from git's list. Installs into different
/// worktrees never wait on each other. Entries nobody holds are dropped on
/// the way, so the map stays as small as the set of in-flight installs.
fn worktree_config_lock(key: &Path) -> std::sync::Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, std::sync::Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| std::sync::Arc::strong_count(lock) > 1);
    locks.entry(key.to_path_buf()).or_default().clone()
}

/// Install the session's helper for `working_dir` ONLY, scoped to the coord
/// URL (`scope`, from [`credential_url_scope`]).
///
/// The keys go to the worktree's own `config.worktree` (`git config
/// --worktree`), never the repository's shared `.git/config`: from a linked
/// worktree `--local` writes the SHARED file, so parallel sessions in sibling
/// worktrees replaced each other's helper, and a dead runner's helper was
/// inherited by every checkout of the repo (plan
/// 2026-11-26-runner-credential-helper-written-to-the-shared-repo-config-clobbers-parallel-worktree-sessions).
/// The same path is used for a primary checkout, whose `.git/config.worktree`
/// linked worktrees do not read. A failed `--worktree` write fails the install;
/// there is deliberately no fallback to `--local`.
///
/// `credential.<scope>.helper` is a list and git reads the shared config
/// before `config.worktree`, so an EMPTY reset entry is kept FIRST in this
/// file: it clears whatever the shared config (a stale pre-worktree install,
/// a dead runner) contributed for the coord host before ours is consulted.
/// Ours is APPENDED (`--add`), because one worktree can hold two installs;
/// removal is [`unset_orphaned_install`]'s exact token-file match. Scoping to
/// the coord host leaves GitHub/GCM helpers untouched for this worktree — the
/// binary answers only the coord host anyway.
fn set_git_credential_helper(
    working_dir: &Path,
    scope: &str,
    binary_path: &Path,
    config_path: &Path,
) -> Result<(), String> {
    let binary_str = binary_path.to_string_lossy().replace('\\', "/");
    let config_str = config_path.to_string_lossy().replace('\\', "/");
    let helper_value = format!("{binary_str} --config {config_str}");
    let helper_key = scoped_credential_key(scope, HELPER_VAR);

    ensure_worktree_config_enabled(working_dir)?;

    // Keyed by the file the writes land in; a dir whose file did not resolve
    // (the writes below then fail anyway) is keyed by itself.
    let lock_key = git_local_config_file(working_dir).unwrap_or_else(|| working_dir.to_path_buf());
    let lock = worktree_config_lock(&lock_key);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let existing = {
        let out = git_in(
            working_dir,
            &["config", "--worktree", "--get-all", &helper_key],
        )?;
        match out.status.code() {
            Some(0) => parse_get_all(&out.stdout),
            Some(1) => Vec::new(),
            code => {
                return Err(format!(
                    "git config --worktree --get-all {helper_key} failed ({code:?}): {}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
        }
    };
    if existing.first().map(String::as_str) != Some("") {
        // The reset must LEAD the list. Values that are already there (only
        // possible from a hand edit) are kept, after it, in their order.
        if !existing.is_empty() {
            git_config_worktree(working_dir, &["--unset-all", &helper_key])?;
        }
        git_config_worktree(working_dir, &["--add", &helper_key, ""])?;
        for value in existing.iter().filter(|v| !v.is_empty()) {
            git_config_worktree(working_dir, &["--add", &helper_key, value])?;
        }
    }
    if !existing.contains(&helper_value) {
        git_config_worktree(working_dir, &["--add", &helper_key, &helper_value])?;
    }
    // Without useHttpPath git never sends `path=` to the helper, and the
    // helper's repo-registry lookup is keyed on the request path — the
    // install would be a production no-op.
    git_config_worktree(
        working_dir,
        &[&scoped_credential_key(scope, USE_HTTP_PATH_VAR), "true"],
    )?;

    Ok(())
}

/// Bound interactive pushes from this worktree: abort any HTTP transfer
/// trickling below 1 KiB/s for 60s. Without these a `git push` against
/// a genuinely stalled server hangs indefinitely (2026-07-12 incident:
/// coord git door 503-refused every push for ~5.5h). Best-effort, same
/// posture as the credential.helper write.
fn set_git_low_speed_bounds(working_dir: &Path) -> Result<(), String> {
    for (key, value) in [("http.lowSpeedLimit", "1024"), ("http.lowSpeedTime", "60")] {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.args([
            "-C",
            &working_dir.to_string_lossy(),
            "config",
            "--local",
            key,
            value,
        ]);
        let output = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
            .map_err(|e| format!("run git config {key}: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git config --local {key} failed: {stderr}"));
        }
    }

    Ok(())
}

/// Derive the credential URL scope (`<scheme>://<host>[:port]`) from a coord
/// base URL. Handles a trailing slash and keeps non-default ports (e.g.
/// `http://localhost:9870`). Reuses reqwest's URL parser rather than
/// hand-rolling a second one.
fn credential_url_scope(coord_base: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(coord_base.trim())
        .map_err(|e| format!("parse coord base {coord_base:?}: {e}"))?;
    match url.scheme() {
        // Url::origin() drops any path/query and omits default ports, which
        // is exactly the scope git's credential.<url>.* matching wants.
        "http" | "https" => Ok(url.origin().ascii_serialization()),
        other => Err(format!(
            "coord base {coord_base:?} has non-http scheme {other:?}"
        )),
    }
}

/// Build a `git config` command targeting either the global config
/// (production) or an explicit file (tests). `--file` lets tests exercise the
/// real read-detect-write logic against a temp config without touching the
/// machine's `~/.gitconfig` and without `std::env::set_var` (which races
/// under the parallel test harness).
fn git_config_cmd(config_file: Option<&Path>) -> std::process::Command {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("config");
    match config_file {
        Some(path) => {
            cmd.arg("--file").arg(path);
        }
        None => {
            cmd.arg("--global");
        }
    }
    cmd
}

/// The values of a successful `git config --get-all`, one per line.
fn parse_get_all(stdout: &[u8]) -> Vec<String> {
    let stdout = String::from_utf8_lossy(stdout);
    let mut values: Vec<String> = stdout
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    // Drop the artifact of the final newline — but only one element, so a
    // genuinely empty-valued last entry ("key =") survives.
    if values.last().is_some_and(|l| l.is_empty()) {
        values.pop();
    }
    values
}

fn git_config_get_all_at(config_file: Option<&Path>, key: &str) -> Result<Vec<String>, String> {
    let mut cmd = git_config_cmd(config_file);
    cmd.args(["--get-all", key]);
    let output = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
        .map_err(|e| format!("run git config --get-all {key}: {e}"))?;

    if output.status.success() {
        return Ok(parse_get_all(&output.stdout));
    }

    // Exit code 1 = key not present (also what a not-yet-created --file
    // target yields). Anything else is a real failure.
    if output.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "git config --get-all {key} failed ({:?}): {stderr}",
        output.status.code()
    ))
}

fn git_config_add_at(config_file: Option<&Path>, key: &str, value: &str) -> Result<(), String> {
    let mut cmd = git_config_cmd(config_file);
    cmd.args(["--add", key, value]);
    let output = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
        .map_err(|e| format!("run git config --add {key}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git config --add {key} failed: {stderr}"));
    }
    Ok(())
}

fn git_config_unset_all_at(config_file: Option<&Path>, key: &str) -> Result<(), String> {
    let mut cmd = git_config_cmd(config_file);
    cmd.args(["--unset-all", key]);
    let output = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
        .map_err(|e| format!("run git config --unset-all {key}: {e}"))?;
    match output.status.code() {
        // 5 = "you try to unset an option which does not exist" — fine, the
        // desired end state (key absent) already holds.
        Some(0) | Some(5) => Ok(()),
        code => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "git config --unset-all {key} failed ({code:?}): {stderr}"
            ))
        }
    }
}

/// Idempotently scrub the global git credential config for the coord host.
///
/// The system gitconfig sets `credential.helper=manager`, so Git Credential
/// Manager is consulted first for EVERY host — including coord, where GCM has
/// no account and pops an interactive VS Code / terminal password prompt.
/// Hand-added `provider=generic` / `interactive=false` keys demonstrably do
/// NOT suppress that dialog; the standard-git fix is an EMPTY
/// `credential.<url>.helper` entry, which clears all previously-defined
/// helpers for that URL scope while the repo-local qontinui helper (later in
/// config precedence) still applies. Git APPENDS helpers rather than replacing
/// them, so the empty entry must come FIRST — that ordering is the whole
/// mechanism, and it is why two honest field reports of
/// `!gh auth git-credential` ("works" / "does not work") were both correct.
///
/// The two hint keys are still DELETED, and the reason is now stronger than
/// "they do not work": once the empty reset exists, GCM is never consulted for
/// the coord host at all, so those keys govern nothing. They are DEAD config
/// that LOOKS like a fix — which is precisely how the earlier stopgaps
/// accreted. This function addresses prompt layer 1 (GCM's own UI) for one
/// host; layers 2 and 3 (git's askpass chain and its terminal prompt) are
/// closed for every host by the spawn-env posture in
/// [`non_interactive_git_env`].
///
/// `config_file`: `None` targets `--global` (production); `Some(path)`
/// targets `--file <path>` so tests can drive the real logic against a temp
/// config. Best-effort: returns an aggregated error string, never panics.
fn ensure_global_credential_hygiene(
    coord_base: &str,
    config_file: Option<&Path>,
) -> Result<(), String> {
    let url = credential_url_scope(coord_base)?;
    let mut errors: Vec<String> = Vec::new();

    // 1. Empty-helper reset. Git runs helpers in config order and an empty
    //    entry resets the list at that point, so any pre-existing NON-empty
    //    entry for this scope is deliberately left alone — the reset only
    //    needs the empty entry to EXIST, and `--add` appends without
    //    clobbering. Read first so the steady path performs zero writes
    //    (no .gitconfig mtime churn).
    let helper_key = format!("credential.{url}.helper");
    match git_config_get_all_at(config_file, &helper_key) {
        Ok(values) if values.iter().any(|v| v.is_empty()) => {
            debug!("credential_helper: empty-helper reset already present for {url}");
        }
        Ok(_) => {
            if let Err(e) = git_config_add_at(config_file, &helper_key, "") {
                errors.push(e);
            }
        }
        Err(e) => errors.push(e),
    }

    // 2. Drop the hand-added GCM hint keys — they do not suppress the prompt
    //    and only add confusion. Read-first keeps the steady path write-free.
    for suffix in ["provider", "interactive"] {
        let key = format!("credential.{url}.{suffix}");
        match git_config_get_all_at(config_file, &key) {
            Ok(values) if values.is_empty() => {}
            Ok(_) => {
                if let Err(e) = git_config_unset_all_at(config_file, &key) {
                    errors.push(e);
                }
            }
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub async fn setup_credential_helper(working_dir: &str, session_id: &str) {
    let working_path = Path::new(working_dir);
    match is_git_repo(working_path) {
        GitRepoProbe::Yes => {}
        GitRepoProbe::No => {
            debug!("credential_helper: {working_dir} is not a git repo, skipping");
            return;
        }
        // PROCEED on an unknown. Skipping here is not the safe arm: it hands
        // the agent the interactive credential prompts this module exists to
        // eliminate, and the failure is invisible. Attempting the install
        // costs a handful of bounded `git config` children which fail
        // harmlessly (and are reported) outside a work tree — strictly cheaper
        // than a wedged interactive push.
        GitRepoProbe::Unknown => {
            warn!(
                "credential_helper: could not determine whether {working_dir} is a git repo \
                 (the `rev-parse --git-dir` probe degraded) — attempting the helper install \
                 anyway rather than silently leaving this session with interactive prompts"
            );
        }
    }

    // Set the low-speed bounds before the credential-helper install so
    // interactive pushes are bounded even when the helper install is
    // skipped below (binary missing, token fetch failed, no repos).
    if let Err(e) = set_git_low_speed_bounds(working_path) {
        warn!("credential_helper: set http.lowSpeed bounds failed: {e}");
    }

    let binary_path = match credential_helper_binary_path() {
        Some(p) => p,
        None => {
            debug!("credential_helper: binary not found, skipping");
            return;
        }
    };

    let (coord_base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    let scope = match credential_url_scope(&coord_base) {
        Ok(scope) => scope,
        Err(e) => {
            warn!("credential_helper: no credential URL scope for the coord base, skipping: {e}");
            return;
        }
    };

    let (push_token_result, repos_result) = tokio::join!(
        fetch_push_token(&coord_base, session_id),
        fetch_registered_repos(&coord_base),
    );

    let push_token = match push_token_result {
        Ok(t) => t,
        Err(e) => {
            debug!("credential_helper: failed to fetch push token: {e}");
            return;
        }
    };

    let repos = match repos_result {
        Ok(r) => r,
        Err(e) => {
            debug!("credential_helper: failed to fetch registered repos: {e}");
            return;
        }
    };

    if repos.is_empty() {
        debug!("credential_helper: no registered repos, skipping");
        return;
    }

    let config_path = config_file_path(session_id);
    // NOTE: the helper binary emits ONLY username/password (no
    // protocol/host/path rewrite). coord_url is NOT emitted back to git —
    // the helper uses it purely to scope the answer to the coord host:
    // with credential.useHttpPath=true git consults the helper for EVERY
    // host a registered repo talks to (github.com included), and without
    // the host check coord credentials would leak to those hosts.
    let local_config = git_local_config_file(working_path);
    let created_config = match write_session_config(
        &config_path,
        &coord_base,
        &push_token,
        &repos,
        local_config.as_deref(),
    ) {
        Ok(created) => created,
        Err(e) => {
            warn!("credential_helper: write config file failed: {e}");
            return;
        }
    };

    // Several bounded git children behind a per-worktree lock: off the async
    // worker, so a contended `config.lock` parks a blocking-pool thread, not
    // a tokio worker.
    let install = {
        let (dir, scope, binary, config) = (
            working_path.to_path_buf(),
            scope.clone(),
            binary_path.clone(),
            config_path.clone(),
        );
        qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            set_git_credential_helper(&dir, &scope, &binary, &config)
        })
        .await
        .unwrap_or_else(|e| Err(format!("helper install task failed: {e}")))
    };
    match install {
        Ok(()) => {
            info!(
                session_id = session_id,
                config = %config_path.display(),
                "credential_helper: installed for {working_dir}"
            );
            // Phase 4 — record the install in the in-process registry (so
            // session close can unset the repo-local config again) and
            // start the push-token refresh loop, deduped per session: a
            // second setup call for the SAME session registers the extra
            // working dir but must not spawn a second loop.
            //
            // Registered BEFORE the global hygiene below, which can take up to
            // three git calls. A sibling install of this session that fails in
            // that window consults the registry in `discard_failed_install`;
            // seeing this install there is what stops it from stripping a
            // shared config's helper and deleting the token file under us.
            let spawn_refresh = {
                let mut reg = registry().lock().expect("credential registry poisoned");
                let entry = reg.entry(session_id.to_string()).or_default();
                let dir = working_path.to_path_buf();
                if !entry.dirs.contains(&dir) {
                    entry.dirs.push(dir);
                }
                let spawn = !entry.refresh_running;
                entry.refresh_running = true;
                spawn
            };
            if spawn_refresh {
                tokio::spawn(refresh_loop(
                    coord_base.clone(),
                    session_id.to_string(),
                    config_path.clone(),
                ));
            }
            // Best-effort global hygiene: keep Git Credential Manager (from
            // the system gitconfig) from popping an interactive password
            // prompt for the coord host. Never aborts setup.
            if let Err(e) = ensure_global_credential_hygiene(&coord_base, None) {
                warn!("credential_helper: global credential hygiene failed (non-fatal): {e}");
            }
        }
        Err(e) => {
            warn!("credential_helper: git config failed: {e}");
            // git children under the config and worktree locks: off the
            // async worker, like the install itself.
            let (session, config, local) = (
                session_id.to_string(),
                config_path.clone(),
                local_config.clone(),
            );
            if let Err(e) =
                qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                    discard_failed_install(&session, &config, local.as_deref(), created_config)
                })
                .await
            {
                warn!("credential_helper: discarding the failed install did not finish: {e}");
            }
        }
    }
}

/// Undo what a FAILED [`set_git_credential_helper`] left behind, without
/// breaking another install of the same session.
///
/// The install writes `credential.helper` before `credential.useHttpPath`, so
/// a failure on the second write leaves the helper key naming the token file
/// while the dir never reaches the registry. Deleting the file alone would
/// leave that key with no record anywhere: the "Cannot prompt" state this
/// module's sweep exists to clear. So, under the config lock:
///
/// - If this call did not create the file, or any other install of the
///   session already succeeded (it is in the registry), or another git config
///   has been recorded since, the file is still needed: leave everything.
/// - If our git config path never resolved, there is no way to name where a
///   helper value may have landed: keep the file rather than erase evidence.
/// - Otherwise remove our own helper value first, and delete the file only if
///   that worked. On failure the file and its `git_configs` record stay, and
///   the boot sweep retries once the file is old enough.
fn discard_failed_install(
    session_id: &str,
    config_path: &Path,
    local_config: Option<&Path>,
    created_config: bool,
) {
    if !created_config {
        return;
    }
    let _guard = config_file_lock();
    let session_live = registry()
        .lock()
        .expect("credential registry poisoned")
        .get(session_id)
        .is_some_and(|state| !state.dirs.is_empty());
    if session_live {
        return;
    }
    let ours: Vec<String> = local_config
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    if !recorded_git_configs(config_path)
        .iter()
        .all(|entry| ours.contains(entry))
    {
        return;
    }
    let Some(local_config) = local_config else {
        // The config path never resolved (`rev-parse` timed out or failed), so
        // a helper value may have landed with no way to name where. Deleting
        // the token file would only erase the evidence; leave it.
        warn!(
            "credential_helper: failed install with an unresolved git config; keeping {}",
            config_path.display()
        );
        return;
    };
    if unset_orphaned_install(local_config, config_path) == OrphanUnset::Failed {
        warn!(
            "credential_helper: could not undo the partial helper install in {}; \
             keeping {} so the boot sweep can retry",
            local_config.display(),
            config_path.display()
        );
        return;
    }
    let _ = std::fs::remove_file(config_path);
}

pub async fn setup_credential_helper_for_worktree(worktree_path: &Path, session_id: &str) {
    let dir_str = worktree_path.to_string_lossy().to_string();
    setup_credential_helper(&dir_str, session_id).await;
}

pub use qontinui_runner_lib::git_posture::{
    non_interactive_git_env, prompt_proof_git_env, ASKPASS_DISABLED_SENTINEL,
};

/// Apply [`non_interactive_git_env`] to a `std::process::Command` child.
///
/// One of three thin wrappers over the SAME list — the shape
/// `terminal::scrub_credential_env_*` already uses — so no spawn seam restates
/// a key and a change here reaches every seam at once.
pub(crate) fn apply_non_interactive_git_env_std(cmd: &mut std::process::Command) {
    for (k, v) in non_interactive_git_env() {
        cmd.env(k, v);
    }
}

/// Twin of [`apply_non_interactive_git_env_std`] for the tokio seams.
pub(crate) fn apply_non_interactive_git_env_tokio(cmd: &mut tokio::process::Command) {
    for (k, v) in non_interactive_git_env() {
        cmd.env(k, v);
    }
}

/// Twin of [`apply_non_interactive_git_env_std`] for the PTY seam.
///
/// `CommandBuilder` seeds its env map at construction from the process env
/// (plus, on Windows, the HKLM/HKCU `Environment` registry keys), so this
/// genuinely REPLACES an inherited `GIT_ASKPASS` — the shape of coord finding
/// `0056361d`, where VS Code handed the runner an askpass the runner passed
/// straight through.
pub(crate) fn apply_non_interactive_git_env_pty(cmd: &mut portable_pty::CommandBuilder) {
    for (k, v) in non_interactive_git_env() {
        cmd.env(k, v);
    }
}

/// Test-only assertion: a spawn seam applied the full [`non_interactive_git_env`]
/// posture to its child's environment.
///
/// Shared by the per-seam call-site tests so every seam asserts the SAME key
/// set from the SAME source — a key added to the posture is immediately
/// required at every seam rather than at whichever ones someone remembered.
/// Mirrors `terminal::assert_credentials_scrubbed_std` in shape and intent.
#[cfg(test)]
pub(crate) fn assert_non_interactive_git_posture(get: impl Fn(&str) -> Option<String>, seam: &str) {
    for (k, v) in non_interactive_git_env() {
        assert_eq!(
            get(&k).as_deref(),
            Some(v.as_str()),
            "{seam}: {k} is not set to the non-interactive git posture value — \
             this spawn seam can still reach a credential prompt and hang"
        );
    }
}

/// [`assert_non_interactive_git_posture`] over a `std::process::Command`.
#[cfg(test)]
pub(crate) fn assert_non_interactive_git_posture_std(cmd: &std::process::Command, seam: &str) {
    let envs: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_string(),
                v.map(|v| v.to_string_lossy().to_string()),
            )
        })
        .collect();
    assert_non_interactive_git_posture(
        |k| {
            envs.iter()
                .find(|(key, _)| key == k)
                .and_then(|(_, v)| v.clone())
        },
        seam,
    );
}

/// [`assert_non_interactive_git_posture`] over a `tokio::process::Command`.
#[cfg(test)]
pub(crate) fn assert_non_interactive_git_posture_tokio(cmd: &tokio::process::Command, seam: &str) {
    assert_non_interactive_git_posture_std(cmd.as_std(), seam);
}

/// [`assert_non_interactive_git_posture`] over a PTY `CommandBuilder`.
#[cfg(test)]
pub(crate) fn assert_non_interactive_git_posture_pty(
    cmd: &portable_pty::CommandBuilder,
    seam: &str,
) {
    assert_non_interactive_git_posture(
        |k| cmd.get_env(k).map(|v| v.to_string_lossy().to_string()),
        seam,
    );
}

/// Outcome of one refresh-loop tick. Factored into a type (rather than a
/// bare bool) so the loop's exit signal is explicit.
#[derive(Debug, PartialEq, Eq)]
enum TickOutcome {
    /// Config rewritten with a fresh token.
    Refreshed,
    /// Config file no longer exists (cleanup or the startup sweep removed
    /// it), or is the token-less tombstone a failed cleanup left — the loop
    /// must exit.
    ConfigGone,
}

/// One tick of the token refresh loop, factored out of [`refresh_loop`] so
/// tests can drive it with an injected `fetch` closure (no network, no env
/// mutation).
///
/// Re-reads the CURRENT config file and swaps only `push_token`; `coord_url`
/// and `repos` are deliberately reused rather than re-fetched — the
/// canonical-repo set changes only on operator action while the token
/// expires every 15 minutes, and reading the live file (instead of values
/// captured when the loop spawned) also preserves any newer repo list a
/// later re-setup wrote for the same session.
async fn refresh_tick<F, Fut>(config_path: &Path, fetch: F) -> Result<TickOutcome, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    if !config_path.exists() || is_tombstone(config_path) {
        return Ok(TickOutcome::ConfigGone);
    }
    let fresh_token = fetch().await?;
    // Read-modify-write under the config lock, with no await inside it, so a
    // concurrent `write_session_config` for another dir of the same session
    // cannot land between the read and the write and have its `git_configs`
    // entry overwritten.
    let _guard = config_file_lock();
    let raw = match std::fs::read_to_string(config_path) {
        Ok(raw) => raw,
        // Raced with cleanup between the exists() check and the read — same
        // exit signal as the up-front check.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TickOutcome::ConfigGone),
        Err(e) => return Err(format!("read {}: {e}", config_path.display())),
    };
    let mut config: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse config: {e}"))?;
    // A file with no token is the tombstone a failed cleanup leaves for the
    // boot sweep: the session is closed, so the loop must exit, not revive it.
    if config.get("push_token").is_none() {
        return Ok(TickOutcome::ConfigGone);
    }
    config["push_token"] = json!(fresh_token);
    atomic_overwrite(config_path, &config.to_string())?;
    Ok(TickOutcome::Refreshed)
}

/// Atomically replace `path` with `contents`: write a sibling temp file in
/// the SAME directory, then rename it over the original. The helper binary
/// may read the config at any instant, so the file must never be observable
/// half-written; same-volume rename is atomic (on Windows `std::fs::rename`
/// maps to `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, which replaces an
/// existing destination). The rename CAN still fail with a Windows sharing
/// violation if the helper happens to hold the destination open without
/// FILE_SHARE_DELETE at that instant — fall back to remove+rename with brief
/// retries: the reader's open window is one small read (microseconds), and
/// between remove and rename the helper sees "no config" and declines to
/// answer, which is fail-safe (no stale token is ever served).
fn atomic_overwrite(path: &Path, contents: &str) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("config path has no file name: {}", path.display()))?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("config path has no parent dir: {}", path.display()))?;
    let tmp = dir.join(format!("{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents).map_err(|e| format!("write temp {}: {e}", tmp.display()))?;

    if std::fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }

    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            // Rare fallback path; a bounded blocking sleep beats pulling an
            // async signature through every (sync) caller.
            std::thread::sleep(Duration::from_millis(25));
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                last_err = format!("remove {}: {e}", path.display());
                continue;
            }
        }
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = format!("rename {} -> {}: {e}", tmp.display(), path.display()),
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(format!("atomic overwrite failed after retries: {last_err}"))
}

/// Per-session background loop that re-mints the coord push token before it
/// expires (coord TTL 15 min, refresh cadence 10 min). Exits when:
/// (a) the config file no longer exists — [`cleanup_credential_helper`] ran
///     or the startup sweep removed it; or
/// (b) [`MAX_CONSECUTIVE_REFRESH_FAILURES`] ticks fail in a row — repeated
///     failure means the session is gone on coord or device auth is revoked
///     (terminal; nothing this loop can fix). A single transient failure
///     just skips the tick.
/// Write failures count toward the same consecutive-failure budget as fetch
/// failures: a config file we can never rewrite is equally terminal.
async fn refresh_loop(coord_base: String, session_id: String, config_path: PathBuf) {
    let mut consecutive_failures: u32 = 0;
    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;
        let base = coord_base.clone();
        let sid = session_id.clone();
        match refresh_tick(&config_path, move || async move {
            fetch_push_token(&base, &sid).await
        })
        .await
        {
            Ok(TickOutcome::Refreshed) => {
                consecutive_failures = 0;
                debug!(
                    session_id = %session_id,
                    "credential_helper: push token refreshed"
                );
            }
            Ok(TickOutcome::ConfigGone) => {
                debug!(
                    session_id = %session_id,
                    "credential_helper: config file gone — refresh loop exiting"
                );
                break;
            }
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_REFRESH_FAILURES {
                    warn!(
                        session_id = %session_id,
                        "credential_helper: {consecutive_failures} consecutive refresh failures (last: {e}) — refresh loop exiting"
                    );
                    break;
                }
                debug!(
                    session_id = %session_id,
                    "credential_helper: refresh tick failed (transient, {consecutive_failures}/{MAX_CONSECUTIVE_REFRESH_FAILURES}): {e}"
                );
            }
        }
    }
    // Clear the dedupe bit so a later re-setup for this session can spawn a
    // fresh loop. Cleanup may already have removed the whole entry — fine.
    if let Some(entry) = registry()
        .lock()
        .expect("credential registry poisoned")
        .get_mut(&session_id)
    {
        entry.refresh_running = false;
    }
}

/// Serialises every read-modify-write of a session token config file in this
/// process: [`write_session_config`] (one call per working dir, and the
/// worktree materialiser runs those concurrently for one session) and
/// [`refresh_tick`]. One lock for all files: writes are rare and tiny.
/// Poisoning is ignored, since the guarded data is a file, not the `()`.
fn config_file_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write a session's token config, adding `local_config` to the
/// `git_configs` it already records. Returns whether the file was CREATED by
/// this call.
///
/// `git_configs` lists the git config FILES the helper keys were written into
/// (`git rev-parse --git-path config.worktree`), not the working dirs. The
/// in-process registry drives teardown while the runner lives, but it dies
/// with the process, so a crash leaves those keys behind pointing at a token
/// file nothing will refresh. With the files recorded here,
/// [`sweep_stale_cred_files`] can unset the keys when it deletes the token
/// file. It has to be the config FILE: by the time the sweep runs the worktree
/// itself is usually deleted (and an older install, written with `--local`,
/// recorded the MAIN checkout's shared config, which the same removal reads).
///
/// Read-merge-write happens under [`config_file_lock`] and the write is
/// atomic, so concurrent setups for one session cannot drop each other's
/// entries or observe a truncated file.
fn write_session_config(
    config_path: &Path,
    coord_url: &str,
    push_token: &str,
    repos: &[String],
    local_config: Option<&Path>,
) -> Result<bool, String> {
    let _guard = config_file_lock();
    let created = !config_path.exists();
    let mut git_configs = recorded_git_configs(config_path);
    if let Some(local_config) = local_config {
        let entry = local_config.to_string_lossy().to_string();
        if !git_configs.contains(&entry) {
            git_configs.push(entry);
        }
    }
    let config = json!({
        "coord_url": coord_url,
        "push_token": push_token,
        "repos": repos,
        "git_configs": git_configs,
    });
    atomic_overwrite(config_path, &config.to_string())?;
    Ok(created)
}

/// Escape `s` for use as a literal inside a git `value-pattern` (an extended
/// regex).
fn regex_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if r"\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether a `credential.helper` value names `file_name` as its token config:
/// the value ends with that name, right after a path separator. Anchored so
/// one token file's name can never match inside another's.
fn helper_names_token_file(value: &str, file_name: &str) -> bool {
    let value = value.trim_end();
    value
        .strip_suffix(file_name)
        .is_some_and(|prefix| prefix.ends_with('/') || prefix.ends_with('\\'))
}

/// Every `credential.helper` / `credential.<scope>.helper` entry in the git
/// config FILE `config_file`, as `(key, value)` in file order. The key comes
/// back as git prints it: section and variable lower-cased, the URL scope as
/// written.
fn helper_entries_at(config_file: &Path) -> Result<Vec<(String, String)>, String> {
    let mut cmd = git_config_cmd(Some(config_file));
    // `-z`: `key\nvalue\0` per entry, so a value with spaces (or an empty
    // reset) parses unambiguously.
    cmd.args(["-z", "--get-regexp", r"^credential\.(.*\.)?helper$"]);
    let out = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
        .map_err(|e| format!("run git config --get-regexp helper: {e}"))?;
    if out.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !out.status.success() {
        return Err(format!(
            "git config --get-regexp helper failed ({:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| match chunk.split_once('\n') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (chunk.to_string(), String::new()),
        })
        .collect())
}

/// The `useHttpPath` key that belongs beside helper key `helper_key`
/// (`credential.helper` → `credential.useHttpPath`,
/// `credential.<scope>.helper` → `credential.<scope>.useHttpPath`).
fn use_http_path_key_for(helper_key: &str) -> String {
    let prefix = helper_key
        .get(..helper_key.len().saturating_sub(HELPER_VAR.len()))
        .unwrap_or("credential.");
    format!("{prefix}{USE_HTTP_PATH_VAR}")
}

/// What [`unset_orphaned_install`] did with one recorded git config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanUnset {
    /// Helper value(s) naming the token file were removed.
    Removed,
    /// The config is gone, or no longer names the token file.
    NothingToDo,
    /// git failed or timed out; the keys may still be there.
    Failed,
}

/// Remove the helper keys in the git config file `local_config` that still
/// point at `token_file`, the token config of a session whose teardown never
/// ran (or is running now).
///
/// Only helper values naming this exact token file are removed, under every
/// helper key that carries one: the coord-scoped `credential.<scope>.helper`
/// a current install writes into `config.worktree`, and the unscoped
/// `credential.helper` an older `--local` install left in a shared config. A
/// value another session installed names a different, live file, so it is
/// left untouched. The matching `useHttpPath` is unset only when no qontinui
/// helper remains under that key, because a live install needs it. The empty
/// reset entry a scoped install keeps first is left in place: alone it only
/// stops a shared-config helper being tried for the coord host in this
/// worktree. Operates on the FILE with `--file`, so it works after the
/// worktree that wrote the keys has been deleted. Errors are debug!-logged and
/// reported as [`OrphanUnset::Failed`], so the caller can keep the record and
/// retry.
fn unset_orphaned_install(local_config: &Path, token_file: &Path) -> OrphanUnset {
    let Some(file_name) = token_file.file_name().and_then(|n| n.to_str()) else {
        return OrphanUnset::NothingToDo;
    };
    if !local_config.is_file() {
        return OrphanUnset::NothingToDo;
    }
    // The same per-file lock an install into this file holds (keyed by the
    // same `git_local_config_file` path the install records), so a teardown
    // and an install in one worktree never contend on its `config.lock`. No
    // caller holds it here: installs release it before any removal runs.
    let lock = worktree_config_lock(local_config);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let shown = local_config.display();
    let entries = match helper_entries_at(local_config) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("credential_helper: sweep read helpers in {shown}: {e}");
            return OrphanUnset::Failed;
        }
    };
    let mut keys: Vec<String> = Vec::new();
    for (key, value) in &entries {
        if helper_names_token_file(value, file_name) && !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    if keys.is_empty() {
        return OrphanUnset::NothingToDo;
    }

    // The same anchoring as `helper_names_token_file`, as a git value-pattern
    // (extended regex): a path separator, the literal name, end of value.
    let pattern = format!(r"[/\\]{}$", regex_literal(file_name));
    for key in &keys {
        let mut cmd = git_config_cmd(Some(local_config));
        cmd.args(["--unset-all", key, &pattern]);
        // Exit 5 ("no value matched") counts as a failure here: the read above
        // just found a matching value, so git disagreeing means the key may
        // still be there (e.g. a hand-quoted value with trailing whitespace
        // that `$` does not reach). Keeping the record beats losing it.
        match crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT) {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                debug!(
                    "credential_helper: sweep unset {key} in {shown} failed ({:?}): {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                );
                return OrphanUnset::Failed;
            }
            Err(e) => {
                debug!("credential_helper: sweep unset {key} in {shown}: {e}");
                return OrphanUnset::Failed;
            }
        }

        // Not atomic with the removal above: a sibling session installing into
        // the same file between the two git calls can lose its `useHttpPath`.
        // Closing that window would take a lock git's own `config.lock` does
        // not offer across two commands; the window is two child processes
        // wide, and the old blanket unset lost it every time.
        //
        // Deliberately NOT `is_qontinui_helper_binary`: this check decides
        // whether to KEEP `useHttpPath`, so it must err towards "something that
        // might be ours remains" — a `!`-wrapped, quoted or otherwise unusual
        // value naming the helper anywhere still counts. The stem test would be
        // less conservative here.
        let qontinui_helper_remains = git_config_get_all_at(Some(local_config), key)
            .map(|values| values.iter().any(|v| v.contains(HELPER_BINARY_STEM)))
            .unwrap_or(true);
        if !qontinui_helper_remains {
            let use_http_path = use_http_path_key_for(key);
            if let Err(e) = git_config_unset_all_at(Some(local_config), &use_http_path) {
                debug!("credential_helper: sweep unset {use_http_path} in {shown}: {e}");
                return OrphanUnset::Failed;
            }
        }
    }
    OrphanUnset::Removed
}

/// `git rev-parse --git-path <name>` for `dir`, as an absolute path. `None`
/// when `dir` is not a repo or git did not answer in time.
fn git_path_file(dir: &Path, name: &str) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    let out = git_in(dir, &["rev-parse", "--git-path", name]).ok()?;
    if !out.status.success() {
        return None;
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rel.is_empty() {
        return None;
    }
    let path = PathBuf::from(&rel);
    let path = if path.is_absolute() {
        path
    } else {
        dir.join(path)
    };
    // Recorded for a sweep that may run in another process much later, so
    // store an absolute path. Not `fs::canonicalize`: on Windows that yields
    // a `\\?\` verbatim path, which git does not reliably accept for `--file`.
    Some(std::path::absolute(&path).unwrap_or(path))
}

/// The git config FILE that `git config --worktree` writes for `dir`
/// (`git rev-parse --git-path config.worktree`): `.git/worktrees/<id>/config.worktree`
/// for a linked worktree, `.git/config.worktree` for a primary checkout. This
/// is where [`set_git_credential_helper`] puts the keys, and so what
/// recording, cleanup, [`discard_failed_install`] and the boot sweep target.
fn git_local_config_file(dir: &Path) -> Option<PathBuf> {
    git_path_file(dir, "config.worktree")
}

/// The repository's SHARED config file for `dir` (`.git/config`, read by
/// every worktree of the repo). For a primary checkout it is read without a
/// git child, which keeps the per-cycle repair below cheap on the steady path.
/// `None` unless `dir` is a checkout ROOT (has a `.git` of its own), so a
/// directory inside some other repository never resolves to that repository.
fn git_shared_config_file(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    if !dot_git.exists() {
        return None;
    }
    if dot_git.is_dir() {
        let path = dot_git.join("config");
        return Some(std::path::absolute(&path).unwrap_or(path));
    }
    git_path_file(dir, "config")
}

/// One side of a helper value as a path: surrounding whitespace and one pair
/// of matching quotes stripped.
fn unquote_path(raw: &str) -> &str {
    let raw = raw.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = raw
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    raw
}

/// Whether `binary` (read out of a helper value, unquoted) is the qontinui
/// helper: judged by its file stem, `.exe` or not. The one "is this ours"
/// test for judging a helper dead.
fn is_qontinui_helper_binary(binary: &Path) -> bool {
    binary.file_stem().and_then(|s| s.to_str()) == Some(HELPER_BINARY_STEM)
}

/// Whether a path read out of a helper value is PROVABLY gone: only an
/// absolute path that does not exist. A relative or bare name (resolved
/// through `PATH` or the cwd by git) cannot be judged from here, so it is
/// never dead.
/// An existence check that FAILS (permissions, I/O) cannot judge either.
fn absolute_and_missing(path: &Path) -> bool {
    path.is_absolute() && matches!(path.try_exists(), Ok(false))
}

/// Why a `qontinui-git-credential` helper value is DEAD, with the token file
/// it names: `None` when it is not ours, names no token file (nothing
/// [`unset_orphaned_install`] could match), is alive, or cannot be judged.
///
/// The value is split at ` --config `. A `!`-prefixed value is a shell
/// snippet whose binary git does not resolve as a path, so it is UNKNOWN and
/// kept; so is a binary that is not an absolute path. Quotes around either
/// side are stripped before it is judged.
fn dead_qontinui_helper(value: &str) -> Option<(PathBuf, &'static str)> {
    if value.trim_start().starts_with('!') {
        return None;
    }
    let (binary, token) = value.split_once(" --config ")?;
    let binary = Path::new(unquote_path(binary));
    if !is_qontinui_helper_binary(binary) {
        return None;
    }
    let token = unquote_path(token);
    if token.is_empty() {
        return None;
    }
    let token = PathBuf::from(token);
    if absolute_and_missing(binary) {
        Some((token, "its binary is gone"))
    } else if absolute_and_missing(&token) {
        Some((token, "its token file is gone"))
    } else {
        None
    }
}

/// The (shared config, helper value) pairs already reported as dead but
/// unremovable, so the publish cycle logs each at info only once.
fn unmatched_dead_helpers_seen() -> &'static Mutex<std::collections::HashSet<(PathBuf, String)>> {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<(PathBuf, String)>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Remove every DEAD qontinui helper from `checkout`'s shared git config:
/// a `qontinui-git-credential` value (unscoped `credential.helper` or a
/// `credential.<scope>.helper`) whose binary or token file provably no longer
/// exists ([`dead_qontinui_helper`]; a value it cannot judge is kept).
/// Returns how many token files' helpers were removed.
///
/// A runner that is stopped or crashes (a temp runner above all) leaves its
/// helper naming a binary that is gone; while that value sits in the shared
/// config, every fetch in every worktree of the repo — github.com included —
/// prints `qontinui-git-credential: not found`. The boot sweep never repairs
/// it while the primary runner stays up, so the fleet publish cycle calls
/// this for every managed checkout before its fetch. A live install writes
/// its token file before its helper, so a missing token file is as dead as a
/// missing binary. Removal is [`unset_orphaned_install`], which also does the
/// `useHttpPath` bookkeeping; one info line per repair. Best-effort: a git
/// failure is debug!-logged and retried next cycle.
pub(crate) fn repair_dead_shared_helpers(checkout: &Path) -> usize {
    let Some(shared) = git_shared_config_file(checkout) else {
        return 0;
    };
    // Steady path: no qontinui helper in the file, no git child at all.
    match std::fs::read(&shared) {
        Ok(raw)
            if raw
                .windows(HELPER_BINARY_STEM.len())
                .any(|w| w == HELPER_BINARY_STEM.as_bytes()) => {}
        _ => return 0,
    }
    let entries = match helper_entries_at(&shared) {
        Ok(entries) => entries,
        Err(e) => {
            debug!(
                "credential_helper: repair could not read the helpers in {}: {e}",
                shared.display()
            );
            return 0;
        }
    };
    let mut dead_tokens: Vec<(PathBuf, &'static str, &str)> = Vec::new();
    for (_key, value) in &entries {
        let Some((token, reason)) = dead_qontinui_helper(value) else {
            continue;
        };
        if !dead_tokens.iter().any(|(t, _, _)| t == &token) {
            dead_tokens.push((token, reason, value.as_str()));
        }
    }
    let mut repaired = 0usize;
    for (token, reason, value) in dead_tokens {
        match unset_orphaned_install(&shared, &token) {
            OrphanUnset::Removed => {
                repaired += 1;
                info!(
                    "credential_helper: removed a dead qontinui-git-credential helper from {} \
                     ({reason}; token file {})",
                    shared.display(),
                    token.display()
                );
            }
            // Judged dead, yet the exact token-file match found nothing to
            // remove: the value is not in the shape an install writes (e.g. a
            // hand-quoted token). Left alone, but made visible.
            OrphanUnset::NothingToDo => {
                // Info once per (file, value) per process, then debug: this
                // runs every publish cycle and the value stays until a human
                // edits it.
                let first = unmatched_dead_helpers_seen()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert((shared.clone(), value.to_string()));
                if first {
                    info!(
                        "credential_helper: a dead qontinui-git-credential helper in {} \
                         ({reason}) could not be matched for removal and was left in place: \
                         {value:?}",
                        shared.display()
                    );
                } else {
                    debug!(
                        "credential_helper: unmatched dead helper still in {}: {value:?}",
                        shared.display()
                    );
                }
            }
            OrphanUnset::Failed => debug!(
                "credential_helper: repair could not remove the dead helper naming {} from {}; \
                 retrying next cycle",
                token.display(),
                shared.display()
            ),
        }
    }
    repaired
}

/// Tear down everything [`setup_credential_helper`] installed for a session:
/// remove this session's repo-local helper keys from every git config file
/// it wrote, then remove the token config file (whose absence is also the
/// refresh loop's exit signal). Idempotent and best-effort — every failure is
/// logged, never propagated, because this runs inside teardown funnels which
/// must not fail on credential hygiene.
///
/// The keys are removed by [`unset_orphaned_install`], the same exact-match
/// removal the boot sweep uses, over the config FILES the token file recorded
/// plus those the registered dirs resolve to now. Two reasons it is not a
/// blanket `--unset-all` per working dir:
/// - one worktree's `config.worktree` can hold two installs (an agent-id one
///   and a terminal session-id one), and an older `--local` install shares the
///   MAIN checkout's config with every sibling, so a blanket unset strips a
///   helper another session still uses ("Cannot prompt");
/// - a worktree deleted before teardown runs has no dir to `git -C` into,
///   while an older install's keys still sit in the main checkout's config.
///
/// When a removal fails the token file is kept as a tombstone: its
/// `git_configs` record stays for the boot sweep to retry, and its token is
/// dropped, which the helper binary treats as "no answer" and
/// [`refresh_tick`] treats as the loop's exit signal.
///
/// `session_id` is whatever key the matching `setup_credential_helper` call
/// used. Two disjoint key spaces exist and BOTH must be torn down:
/// - the coord **session** UUID (terminal / worker installs), drained by
///   `SessionRegistry::close_by_id`;
/// - the coord **agent** id (agent-worktree installs made inside
///   `allocate_and_materialize_with_claim`), drained by
///   `IsolatedEditContext::drop`.
pub fn cleanup_credential_helper(session_id: &str) {
    let dirs = registry()
        .lock()
        .expect("credential registry poisoned")
        .remove(session_id)
        .map(|state| state.dirs)
        .unwrap_or_default();
    let config_path = config_file_path(session_id);

    // Resolved outside the config lock: each is a bounded `git rev-parse`.
    let mut targets = recorded_git_configs(&config_path);
    let mut any_failed = false;
    for dir in &dirs {
        match git_local_config_file(dir) {
            Some(path) => {
                let path = path.to_string_lossy().to_string();
                if !targets.contains(&path) {
                    targets.push(path);
                }
            }
            // A dir that is gone wrote nothing a `git -C` could reach; its
            // config, if recorded, is already in `targets`. A dir that still
            // exists but did not resolve (a `rev-parse` timeout) may hold keys
            // no one can name, so the token file must not be deleted.
            None if dir.exists() => {
                debug!(
                    "credential_helper: cleanup could not resolve the git config of {}",
                    dir.display()
                );
                any_failed = true;
            }
            None => {}
        }
    }

    for local_config in &targets {
        if unset_orphaned_install(Path::new(local_config), &config_path) == OrphanUnset::Failed {
            any_failed = true;
        }
    }

    // Under the config lock, so a refresh tick that already read the file
    // cannot write it back (with a live token) after this removal.
    let _guard = config_file_lock();
    if !config_path.exists() {
        return;
    }
    if any_failed {
        warn!(
            "credential_helper: cleanup could not remove every helper key naming {}; \
             keeping it without its token so the boot sweep can retry",
            config_path.display()
        );
        // Every config this cleanup tried, not just the recorded ones: a dir's
        // config that never resolved at install time is known only here.
        let mut git_configs = recorded_git_configs(&config_path);
        for target in targets {
            if !git_configs.contains(&target) {
                git_configs.push(target);
            }
        }
        let tombstone = json!({ "git_configs": git_configs });
        if let Err(e) = atomic_overwrite(&config_path, &tombstone.to_string()) {
            debug!("credential_helper: cleanup tombstone write failed: {e}");
        }
        return;
    }
    if let Err(e) = std::fs::remove_file(&config_path) {
        debug!("credential_helper: cleanup config file failed: {e}");
    }
}

/// How long the `Drop`-path cleanup waits for an in-flight install — the
/// default of `IsolatedEditContext`'s per-context cleanup wait.
pub(crate) const CLEANUP_INSTALL_WAIT: Duration = Duration::from_secs(30);

/// [`cleanup_credential_helper`] on a detached thread — the form safe to call
/// from a `Drop` impl.
///
/// Two reasons the direct call is wrong there. (1) Cleanup shells out to
/// `git rev-parse` / `git config` several times per git config; running that inline blocks
/// whichever thread the drop lands on, which for `IsolatedEditContext` is a
/// Tokio worker. (2) Cleanup can panic on a poisoned registry mutex, and a
/// panic raised while unwinding another panic aborts the process — the same
/// hazard `IsolatedEditContext::drop` already guards its claim-release spawn
/// against. A detached `std::thread` needs no runtime (unlike `tokio::spawn`,
/// which itself panics outside one) and contains any panic to itself.
///
/// An install still in flight for `session_id` ([`track_install`]) is waited
/// for first, ON THE SAME THREAD, by polling its completion
/// ([`wait_for_installs`]): bounded by `wait` (production passes
/// [`CLEANUP_INSTALL_WAIT`]), then aborted (with a short grace for the abort to
/// land), and the cleanup runs either way. The thread is unconditional, so a
/// `Drop` during runtime shutdown cannot lose the cleanup the way a task
/// spawned on that runtime could.
pub(crate) fn spawn_cleanup_credential_helper_within(session_id: String, wait: Duration) {
    let pending = take_installs(&session_id);
    std::thread::spawn(move || {
        if !wait_for_installs(&pending, wait) {
            warn!(
                session_id = %session_id,
                "credential_helper: an install was still running after {wait:?} and an abort; \
                 cleaning up anyway"
            );
        }
        cleanup_credential_helper(&session_id);
    });
}

/// Block this (non-runtime) thread until every handle is finished, up to
/// `wait`; then abort what is left and allow it `wait / 10` (at least 100 ms)
/// to land. `true` when all finished.
fn wait_for_installs(pending: &[tokio::task::JoinHandle<()>], wait: Duration) -> bool {
    let poll = |until: std::time::Instant| loop {
        if pending.iter().all(|h| h.is_finished()) {
            return true;
        }
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if poll(std::time::Instant::now() + wait) {
        return true;
    }
    for h in pending {
        h.abort();
    }
    poll(std::time::Instant::now() + abort_grace(wait))
}

/// In-flight credential-helper installs, keyed by the agent id they install
/// under — the same key [`spawn_cleanup_credential_helper_within`] tears down.
fn installs() -> &'static Mutex<HashMap<String, Vec<tokio::task::JoinHandle<()>>>> {
    static INSTALLS: OnceLock<Mutex<HashMap<String, Vec<tokio::task::JoinHandle<()>>>>> =
        OnceLock::new();
    INSTALLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a detached install for `session_id`, so a later removal or cleanup
/// can wait for it ([`settle_installs`], [`spawn_cleanup_credential_helper_within`]).
/// Finished installs already recorded for the id are dropped on the way.
pub(crate) fn track_install(session_id: &str, install: tokio::task::JoinHandle<()>) {
    let mut g = installs().lock().unwrap_or_else(|p| p.into_inner());
    let list = g.entry(session_id.to_string()).or_default();
    list.retain(|h| !h.is_finished());
    list.push(install);
}

fn take_installs(session_id: &str) -> Vec<tokio::task::JoinHandle<()>> {
    installs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(session_id)
        .unwrap_or_default()
}

/// Wait for every install still in flight for `session_id` under ONE shared
/// deadline, `bound` from now, so an agent with several installs cannot stall
/// a refusal report for `bound` per install. Called before an unspawned
/// allocation's trees are removed, so the installs' writes land before (and
/// their credentials are torn down after) the removal. Whatever is still
/// running at the deadline is logged and aborted, and all of it shares one
/// short grace for the aborts to land ([`abort_grace`]). `false` when an
/// install is STILL running after that — the caller must then not judge the
/// trees; that install is put back so the context's cleanup still waits for it.
pub(crate) async fn settle_installs(session_id: &str, bound: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + bound;
    let mut late = Vec::new();
    for mut install in take_installs(session_id) {
        if tokio::time::timeout_at(deadline, &mut install)
            .await
            .is_err()
        {
            late.push(install);
        }
    }
    if late.is_empty() {
        return true;
    }
    warn!(
        session_id = %session_id,
        "credential_helper: {} install(s) still running after {bound:?}; aborting",
        late.len()
    );
    for install in &late {
        install.abort();
    }
    let grace = tokio::time::Instant::now() + abort_grace(bound);
    let mut settled = true;
    for mut install in late {
        if tokio::time::timeout_at(grace, &mut install).await.is_err() {
            warn!(
                session_id = %session_id,
                "credential_helper: an aborted install has not stopped within the grace"
            );
            settled = false;
            track_install(session_id, install);
        }
    }
    settled
}

/// The one grace an abort gets to land: a tenth of the wait, at least 100 ms
/// and at most 2 s.
fn abort_grace(bound: Duration) -> Duration {
    (bound / 10).clamp(Duration::from_millis(100), Duration::from_secs(2))
}

/// Delete stale credential config files (and orphaned `.tmp-*` siblings from
/// a failed atomic rewrite) from `dir`. Returns the number deleted. Factored
/// out of [`spawn_startup_sweep`] so tests can point it at a temp dir with a
/// synthetic `now`. All errors are debug!-logged and skipped.
fn sweep_stale_cred_files(dir: &Path, max_age: Duration, now: SystemTime) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("credential_helper: sweep read_dir {}: {e}", dir.display());
            return 0;
        }
    };
    let mut deleted = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Prefix match covers both the config files themselves
        // (qontinui-git-cred-<session>.json) and any orphaned
        // <name>.json.tmp-<pid> left by an interrupted atomic rewrite.
        if !name.starts_with("qontinui-git-cred-") {
            continue;
        }
        let modified = match entry.metadata().and_then(|m| m.modified()) {
            Ok(m) => m,
            Err(e) => {
                debug!("credential_helper: sweep metadata {name}: {e}");
                continue;
            }
        };
        // An mtime in the future yields Err — leave the file alone.
        let age = match now.duration_since(modified) {
            Ok(age) => age,
            Err(_) => continue,
        };
        if age <= max_age {
            continue;
        }
        // Before deleting a session's token file, remove the helper keys that
        // still point at it from every git config file it recorded. Without
        // this a crashed session leaves `credential.helper` naming a file that
        // no longer exists, and every non-interactive fetch/push in that
        // checkout fails with "Cannot prompt". Files written before
        // `git_configs` existed record nothing and are only deleted, as
        // before. `.tmp-*` siblings carry no install record and are skipped.
        if name.ends_with(".json") {
            let mut any_failed = false;
            for local_config in recorded_git_configs(&entry.path()) {
                match unset_orphaned_install(Path::new(&local_config), &entry.path()) {
                    OrphanUnset::Removed => info!(
                        "credential_helper: sweep removed orphaned helper keys from {local_config} \
                         (they pointed at stale {name})"
                    ),
                    OrphanUnset::NothingToDo => {}
                    OrphanUnset::Failed => any_failed = true,
                }
            }
            // The file is the ONLY record of where the keys are. If any
            // removal failed (a contended config.lock at boot, a git timeout),
            // keep it so the next boot retries. Its token expired long ago,
            // so keeping it serves nothing.
            if any_failed {
                warn!(
                    "credential_helper: sweep kept stale {name}: could not remove the helper \
                     keys it recorded; retrying on the next boot"
                );
                continue;
            }
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => deleted += 1,
            Err(e) => debug!("credential_helper: sweep remove {name}: {e}"),
        }
    }
    deleted
}

/// Whether `config_path` parses as a token config with no `push_token`: the
/// tombstone a failed [`cleanup_credential_helper`] leaves for the boot sweep.
/// A file that cannot be read or parsed is NOT a tombstone, so a transient
/// read failure never ends a live session's refresh loop.
fn is_tombstone(config_path: &Path) -> bool {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|v| v.get("push_token").is_none())
}

/// The `git_configs` a token config file recorded at install time (empty for
/// a file written before the field existed, or one that does not parse).
fn recorded_git_configs(config_path: &Path) -> Vec<String> {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("git_configs").cloned())
        .and_then(|d| serde_json::from_value::<Vec<String>>(d).ok())
        .unwrap_or_default()
}

/// One-shot, best-effort boot sweep of stale credential config files in
/// %TEMP% (files older than [`SWEEP_MAX_AGE`]; younger ones may belong to a
/// live session — possibly another runner instance sharing this temp dir —
/// and are left alone). The age gate assumes a live session's refresh loop
/// keeps touching its file; a loop that gave up after
/// [`MAX_CONSECUTIVE_REFRESH_FAILURES`] stops doing so, and after 24h that
/// session's file and keys are swept like a crashed one. Its token expired
/// long before, so the helper was already serving nothing usable. Runs on a
/// detached thread so it can never slow or fail the boot; errors never
/// propagate (debug!-logged inside the sweep).
pub fn spawn_startup_sweep() {
    std::thread::spawn(|| {
        let deleted =
            sweep_stale_cred_files(&std::env::temp_dir(), SWEEP_MAX_AGE, SystemTime::now());
        if deleted > 0 {
            info!("credential_helper: startup sweep deleted {deleted} stale config file(s)");
        }
    });
}

#[cfg(test)]
mod tests {
    /// Review A4: the `Drop`-path cleanup runs on its own thread AFTER an
    /// install still in flight for the id. The install here writes the
    /// session's config file 300 ms in; a cleanup that ran ahead of it would
    /// leave that file behind for good.
    #[tokio::test]
    async fn the_drop_path_cleanup_waits_for_an_in_flight_install() {
        let id = format!("drop-wait-{}", uuid::Uuid::new_v4());
        let path = config_file_path(&id);
        let written = path.clone();
        let wrote = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wrote_flag = wrote.clone();
        track_install(
            &id,
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                std::fs::write(&written, "{}").unwrap();
                wrote_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        spawn_cleanup_credential_helper_within(id.clone(), Duration::from_secs(10));
        // Keep the runtime driving the install while the cleanup thread waits.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if wrote.load(std::sync::atomic::Ordering::SeqCst) && !path.exists() {
                break;
            }
        }
        // Past the install, give a cleanup that ran AHEAD of it the chance to
        // be seen as one: the file it never deleted would still be there.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let left = path.exists();
        let _ = std::fs::remove_file(&path);
        assert!(
            wrote.load(std::sync::atomic::Ordering::SeqCst),
            "the install wrote its config file"
        );
        assert!(!left, "the cleanup ran after the install and removed it");
    }

    /// One deadline is shared by all of an agent's installs: three that never
    /// finish on their own cost ONE bound plus one abort grace, not three.
    #[tokio::test]
    async fn settling_several_installs_shares_one_deadline() {
        let id = format!("shared-deadline-{}", uuid::Uuid::new_v4());
        for _ in 0..3 {
            track_install(
                &id,
                tokio::spawn(tokio::time::sleep(Duration::from_secs(3600))),
            );
        }
        let started = std::time::Instant::now();
        assert!(settle_installs(&id, Duration::from_millis(300)).await);
        let took = started.elapsed();
        assert!(
            took < Duration::from_millis(600),
            "one shared 300 ms deadline (+ abort grace), not one per install: took {took:?}"
        );
    }

    /// Review A4: an install that never finishes is aborted after the bound
    /// and the cleanup still runs (it is never lost).
    #[tokio::test]
    async fn the_drop_path_cleanup_runs_even_when_an_install_never_finishes() {
        let id = format!("drop-stuck-{}", uuid::Uuid::new_v4());
        let path = config_file_path(&id);
        std::fs::write(&path, "{}").unwrap();
        let install = tokio::spawn(tokio::time::sleep(Duration::from_secs(3600)));
        track_install(&id, install);
        spawn_cleanup_credential_helper_within(id.clone(), Duration::from_millis(100));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while path.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let left = path.exists();
        let _ = std::fs::remove_file(&path);
        assert!(!left, "the cleanup ran despite the stuck install");
    }

    use super::*;

    /// A value from the repository's SHARED config (`git config --local`).
    fn git_config_get(dir: &Path, key: &str) -> Option<String> {
        let out = crate::process_helpers::no_window("git")
            .args(["-C", &dir.to_string_lossy(), "config", "--local", key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The coord scope every test install writes under.
    const TEST_SCOPE: &str = "https://coord.example";

    fn helper_key() -> String {
        scoped_credential_key(TEST_SCOPE, HELPER_VAR)
    }

    fn use_http_path_key() -> String {
        scoped_credential_key(TEST_SCOPE, USE_HTTP_PATH_VAR)
    }

    /// All values of `key` in `dir`'s own `config.worktree`.
    fn worktree_values(dir: &Path, key: &str) -> Vec<String> {
        let file = git_local_config_file(dir).expect("resolve config.worktree");
        git_config_get_all_at(Some(&file), key).unwrap()
    }

    /// Every qontinui helper value in the SHARED config of `dir`'s repo, under
    /// any helper key, scoped or not.
    fn shared_qontinui_helpers(dir: &Path) -> Vec<String> {
        let shared = git_path_file(dir, "config").expect("resolve the shared config");
        helper_entries_at(&shared)
            .unwrap()
            .into_iter()
            .map(|(_, v)| v)
            .filter(|v| v.contains(HELPER_BINARY_STEM))
            .collect()
    }

    /// The helper value an install pointing at `token` writes.
    fn helper_value(token: &Path) -> String {
        format!(
            "C:/bin/qontinui-git-credential.exe --config {}",
            token.to_string_lossy().replace('\\', "/")
        )
    }

    /// The install lands in the checkout's OWN `config.worktree`, scoped to
    /// the coord URL, with the empty reset first; the shared config gains only
    /// `extensions.worktreeConfig`.
    #[test]
    fn set_git_credential_helper_writes_helper_and_use_http_path() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());

        set_git_credential_helper(
            tmp.path(),
            TEST_SCOPE,
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            Path::new("C:\\tmp\\cred.json"),
        )
        .expect("set_git_credential_helper");

        assert_eq!(
            worktree_values(tmp.path(), &helper_key()),
            vec![
                String::new(),
                "C:/bin/qontinui-git-credential.exe --config C:/tmp/cred.json".to_string()
            ]
        );
        // Without useHttpPath git never passes `path=` to the helper, which
        // makes the whole install a production no-op — it must be set.
        assert_eq!(
            worktree_values(tmp.path(), &use_http_path_key()),
            vec!["true".to_string()]
        );
        assert_eq!(
            git_config_get(tmp.path(), "extensions.worktreeConfig").as_deref(),
            Some("true")
        );
        assert!(shared_qontinui_helpers(tmp.path()).is_empty());
        assert_eq!(git_config_get(tmp.path(), "credential.useHttpPath"), None);
    }

    /// A list someone edited by hand (a value, no reset) gets the reset put
    /// back in FRONT, keeping the value after it.
    #[test]
    fn set_git_credential_helper_puts_the_reset_first_in_a_hand_edited_list() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        ensure_worktree_config_enabled(tmp.path()).unwrap();
        git_config_worktree(tmp.path(), &["--add", &helper_key(), "hand-added"]).unwrap();
        let token = tmp.path().join("qontinui-git-cred-hand.json");

        set_git_credential_helper(
            tmp.path(),
            TEST_SCOPE,
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            &token,
        )
        .unwrap();

        assert_eq!(
            worktree_values(tmp.path(), &helper_key()),
            vec![
                String::new(),
                "hand-added".to_string(),
                helper_value(&token)
            ]
        );
    }

    /// git documents `core.bare` / `core.worktree` in the shared config as a
    /// hazard once `extensions.worktreeConfig` is on, so the install refuses
    /// there and writes nothing.
    #[test]
    fn set_git_credential_helper_refuses_a_shared_core_worktree_or_core_bare() {
        for key in ["core.worktree", "core.bare"] {
            let tmp = tempfile::tempdir().unwrap();
            git_init(tmp.path());
            // core.worktree must name a dir that exists, or git itself fails
            // before the check is reached.
            let worktree = tmp.path().to_string_lossy().to_string();
            let value = if key == "core.bare" {
                "true"
            } else {
                worktree.as_str()
            };
            let status = crate::process_helpers::no_window("git")
                .args([
                    "-C",
                    &tmp.path().to_string_lossy(),
                    "config",
                    "--local",
                    key,
                    value,
                ])
                .status()
                .unwrap();
            assert!(status.success());

            let err = set_git_credential_helper(
                tmp.path(),
                TEST_SCOPE,
                Path::new("C:\\bin\\qontinui-git-credential.exe"),
                Path::new("C:\\tmp\\cred.json"),
            )
            .unwrap_err();

            assert!(err.contains(key), "{key}: unexpected error: {err}");
            assert_eq!(
                git_config_get(tmp.path(), "extensions.worktreeConfig"),
                None,
                "{key}: nothing may be written"
            );
        }
    }

    // Collect the env-injected git config into (key -> value) pairs, honoring
    // GIT_CONFIG_COUNT, so a test can assert the LOGICAL config regardless of
    // index offset.
    //
    // The offset is NOT always zero. `non_interactive_git_env` deliberately
    // APPENDS after whatever `GIT_CONFIG_COUNT` the process already inherited,
    // so the pairs it owns run `base..count`, not `0..count`. Reading from 0
    // panics on the inherited indices — which the runner reproduces on itself:
    // `terminal/session.rs` injects exactly this env into every PTY it spawns,
    // so the suite failed for anyone running it from inside a runner terminal
    // while passing in CI's clean environment. Derive `base` the same way the
    // production function does.
    fn injected_git_config(env: &[(String, String)]) -> Vec<(String, String)> {
        let get = |k: &str| -> Option<String> {
            env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone())
        };
        let count: usize = get("GIT_CONFIG_COUNT")
            .expect("GIT_CONFIG_COUNT set")
            .parse()
            .expect("GIT_CONFIG_COUNT numeric");
        let base: usize = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        assert!(
            count >= base,
            "GIT_CONFIG_COUNT {count} must extend the inherited {base}, not shrink it"
        );
        (base..count)
            .map(|i| {
                (
                    get(&format!("GIT_CONFIG_KEY_{i}"))
                        .unwrap_or_else(|| panic!("GIT_CONFIG_KEY_{i}")),
                    get(&format!("GIT_CONFIG_VALUE_{i}"))
                        .unwrap_or_else(|| panic!("GIT_CONFIG_VALUE_{i}")),
                )
            })
            .collect()
    }

    #[test]
    fn non_interactive_git_env_closes_all_three_prompt_layers() {
        let env = non_interactive_git_env();
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };

        // Layer 1 — GCM's own UI (chooser / login dialog).
        assert_eq!(get("GCM_INTERACTIVE"), Some("never"));
        // Layer 3 — git's terminal prompt.
        assert_eq!(get("GIT_TERMINAL_PROMPT"), Some("0"));
        // Layer 2 — git's askpass chain. Measured 2026-09-03 on git 2.47.3:
        // GIT_TERMINAL_PROMPT=0 ALONE hangs indefinitely behind a blocking
        // askpass program, so this is the layer that closes the class.
        assert_eq!(get("GIT_ASKPASS"), Some(ASKPASS_DISABLED_SENTINEL));

        let cfg = injected_git_config(&env);
        assert!(cfg.contains(&(
            "credential.https://github.com.helper".to_string(),
            "!gh auth git-credential".to_string()
        )));
        // One switch per layer: the url-scoped GCM key is a SECOND switch on
        // layer 1 and is deliberately not emitted (accretion is the defect the
        // dossier `git-push-hang-credential-helper` exists to stop).
        assert!(
            !cfg.iter()
                .any(|(k, _)| k == "credential.https://github.com.interactive"),
            "url-scoped GCM interactivity key is redundant with GCM_INTERACTIVE"
        );
        assert!(
            !env.iter()
                .any(|(k, v)| k.starts_with("GIT_CONFIG_VALUE_") && v.is_empty()),
            "no empty-string reset entry (would wipe the --local coord helper)"
        );
    }

    #[test]
    fn non_interactive_git_env_never_emits_an_empty_value() {
        // The posture must never emit an empty env VALUE. An empty
        // `GIT_ASKPASS` would depend on unverifiable Windows `CreateProcess`
        // semantics for a set-but-empty variable, and an empty
        // `GIT_CONFIG_VALUE_n` that arrives as MISSING makes git
        // `die("missing config value ...")` on every invocation in every
        // runner-spawned process. Both failure modes are worse than the hang
        // this posture replaces, which is why the askpass switch is a
        // non-existent PATH and not "".
        for (k, v) in non_interactive_git_env() {
            assert!(
                !v.is_empty(),
                "{k} has an empty value — the posture emits no empty values \
                 (see ASKPASS_DISABLED_SENTINEL)"
            );
        }
    }

    #[test]
    fn set_git_credential_helper_fails_outside_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let err = set_git_credential_helper(
            tmp.path(),
            TEST_SCOPE,
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            Path::new("C:\\tmp\\cred.json"),
        )
        .unwrap_err();
        assert!(err.contains("git config"), "unexpected error: {err}");
    }

    const COORD_URL: &str = "https://coord.qontinui.io";

    #[test]
    fn credential_url_scope_handles_trailing_slash_and_ports() {
        assert_eq!(
            credential_url_scope("https://coord.qontinui.io/").unwrap(),
            "https://coord.qontinui.io"
        );
        assert_eq!(
            credential_url_scope("https://coord.qontinui.io").unwrap(),
            "https://coord.qontinui.io"
        );
        // Non-default port must be kept.
        assert_eq!(
            credential_url_scope("http://localhost:9870").unwrap(),
            "http://localhost:9870"
        );
        assert_eq!(
            credential_url_scope("http://localhost:9870/").unwrap(),
            "http://localhost:9870"
        );
        assert!(credential_url_scope("not a url").is_err());
        assert!(credential_url_scope("file:///etc/passwd").is_err());
    }

    #[test]
    fn hygiene_fresh_config_adds_empty_helper_and_leaves_hints_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("gitconfig");

        // Trailing slash on coord_base must not leak into the scope key.
        ensure_global_credential_hygiene("https://coord.qontinui.io/", Some(&cfg)).unwrap();

        let helpers =
            git_config_get_all_at(Some(&cfg), &format!("credential.{COORD_URL}.helper")).unwrap();
        assert_eq!(helpers, vec![String::new()], "empty-helper reset missing");
        assert!(
            git_config_get_all_at(Some(&cfg), &format!("credential.{COORD_URL}.provider"))
                .unwrap()
                .is_empty()
        );
        assert!(
            git_config_get_all_at(Some(&cfg), &format!("credential.{COORD_URL}.interactive"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn hygiene_already_hygienic_config_performs_zero_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("gitconfig");

        // First pass establishes the hygienic state.
        ensure_global_credential_hygiene(COORD_URL, Some(&cfg)).unwrap();
        let content_before = std::fs::read(&cfg).unwrap();
        let mtime_before = std::fs::metadata(&cfg).unwrap().modified().unwrap();

        // Second pass must be a pure read — no rewrite, no mtime churn.
        ensure_global_credential_hygiene(COORD_URL, Some(&cfg)).unwrap();
        assert_eq!(std::fs::read(&cfg).unwrap(), content_before);
        assert_eq!(
            std::fs::metadata(&cfg).unwrap().modified().unwrap(),
            mtime_before,
            "steady path rewrote the config file"
        );
    }

    #[test]
    fn hygiene_removes_gcm_hints_and_preserves_unrelated_and_nonempty_helper() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("gitconfig");

        let helper_key = format!("credential.{COORD_URL}.helper");
        git_config_add_at(Some(&cfg), &helper_key, "manager").unwrap();
        git_config_add_at(
            Some(&cfg),
            &format!("credential.{COORD_URL}.provider"),
            "generic",
        )
        .unwrap();
        git_config_add_at(
            Some(&cfg),
            &format!("credential.{COORD_URL}.interactive"),
            "false",
        )
        .unwrap();
        // Unrelated keys must survive untouched.
        git_config_add_at(Some(&cfg), "user.name", "keepme").unwrap();
        git_config_add_at(
            Some(&cfg),
            "credential.https://github.com.provider",
            "github",
        )
        .unwrap();

        ensure_global_credential_hygiene(COORD_URL, Some(&cfg)).unwrap();

        // The pre-existing non-empty helper entry is left alone; the empty
        // reset entry is appended after it.
        assert_eq!(
            git_config_get_all_at(Some(&cfg), &helper_key).unwrap(),
            vec!["manager".to_string(), String::new()]
        );
        assert!(
            git_config_get_all_at(Some(&cfg), &format!("credential.{COORD_URL}.provider"))
                .unwrap()
                .is_empty()
        );
        assert!(
            git_config_get_all_at(Some(&cfg), &format!("credential.{COORD_URL}.interactive"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            git_config_get_all_at(Some(&cfg), "user.name").unwrap(),
            vec!["keepme".to_string()]
        );
        assert_eq!(
            git_config_get_all_at(Some(&cfg), "credential.https://github.com.provider").unwrap(),
            vec!["github".to_string()]
        );
    }

    // ---------------------------------------------------------------
    // Phase 4 — refresh tick, startup sweep, cleanup wiring
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn refresh_tick_replaces_token_and_preserves_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("qontinui-git-cred-test.json");
        let initial = json!({
            "coord_url": "https://coord.qontinui.io",
            "push_token": "stale-token",
            "repos": ["qontinui-runner", "qontinui-web"],
        });
        std::fs::write(&config_path, initial.to_string()).unwrap();

        let outcome = refresh_tick(&config_path, || async { Ok("fresh-token".to_string()) })
            .await
            .unwrap();
        assert_eq!(outcome, TickOutcome::Refreshed);

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(after["push_token"], "fresh-token");
        // coord_url and repos must survive the rewrite untouched.
        assert_eq!(after["coord_url"], "https://coord.qontinui.io");
        assert_eq!(after["repos"], json!(["qontinui-runner", "qontinui-web"]));
        // The atomic rewrite must not leave its temp file behind.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn refresh_tick_missing_config_signals_exit_without_fetching() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("qontinui-git-cred-gone.json");

        let fetched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fetched_flag = fetched.clone();
        let outcome = refresh_tick(&config_path, move || {
            fetched_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            async { Err::<String, String>("unused".to_string()) }
        })
        .await
        .unwrap();
        assert_eq!(outcome, TickOutcome::ConfigGone);
        assert!(
            !fetched.load(std::sync::atomic::Ordering::SeqCst),
            "fetch must not run when the config file is gone"
        );
    }

    #[tokio::test]
    async fn refresh_tick_fetch_failure_is_transient_and_leaves_config_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("qontinui-git-cred-err.json");
        let initial = json!({
            "coord_url": "https://coord.qontinui.io",
            "push_token": "still-valid",
            "repos": ["qontinui-runner"],
        })
        .to_string();
        std::fs::write(&config_path, &initial).unwrap();

        let err = refresh_tick(&config_path, || async { Err("401 nope".to_string()) })
            .await
            .unwrap_err();
        assert!(err.contains("401"), "unexpected error: {err}");
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), initial);
    }

    #[test]
    fn sweep_deletes_only_old_matching_files() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("qontinui-git-cred-old.json");
        let fresh = tmp.path().join("qontinui-git-cred-fresh.json");
        let unrelated_old = tmp.path().join("some-other-file.json");
        for p in [&old, &fresh, &unrelated_old] {
            std::fs::write(p, "{}").unwrap();
        }
        // Age `old` and the unrelated file to 25h via mtime (filetime is
        // already a dev-dependency; no clock mocking, no env mutation).
        let old_mtime = filetime::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(25 * 60 * 60),
        );
        filetime::set_file_mtime(&old, old_mtime).unwrap();
        filetime::set_file_mtime(&unrelated_old, old_mtime).unwrap();

        let deleted = sweep_stale_cred_files(tmp.path(), SWEEP_MAX_AGE, SystemTime::now());

        assert_eq!(deleted, 1);
        assert!(!old.exists(), "old matching file should be deleted");
        assert!(fresh.exists(), "fresh file must be left alone");
        assert!(
            unrelated_old.exists(),
            "non-matching file must be left alone"
        );
    }

    fn git_init(dir: &Path) {
        let status = crate::process_helpers::no_window("git")
            .args(["init", "-q", &dir.to_string_lossy()])
            .status()
            .expect("git init");
        assert!(status.success());
    }

    fn age_25h(path: &Path) {
        let old_mtime = filetime::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(25 * 60 * 60),
        );
        filetime::set_file_mtime(path, old_mtime).unwrap();
    }

    /// A repo at `main` with one commit and a linked worktree of it. Returns
    /// the worktree's parent temp dir (keep it alive) and the worktree path.
    fn linked_worktree(main: &Path) -> (tempfile::TempDir, PathBuf) {
        git_init(main);
        let git = |args: &[&str]| {
            let status = crate::process_helpers::no_window("git")
                .arg("-C")
                .arg(main)
                .args(args)
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            // Isolate from the machine's global git config: signing or a
            // global hooks path must not decide whether this fixture builds.
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            "init",
        ]);
        add_linked_worktree(main)
    }

    /// One more linked worktree of the (already committed) repo at `main`.
    fn add_linked_worktree(main: &Path) -> (tempfile::TempDir, PathBuf) {
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt");
        let status = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(main)
            .args(["worktree", "add", "-q", "--detach", &wt.to_string_lossy()])
            .status()
            .expect("git worktree add");
        assert!(status.success());
        (wt_parent, wt)
    }

    /// A per-run session id: cleanup resolves the token file in the real
    /// %TEMP% (production path), so the name must not collide with parallel
    /// test runs.
    fn unique_session_id(tag: &str) -> String {
        format!(
            "test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// Install the helper keys in `dir` pointing at `token`, and record the
    /// install in `token` exactly as `setup_credential_helper` does.
    fn install_and_record(dir: &Path, token: &Path) {
        set_git_credential_helper(
            dir,
            TEST_SCOPE,
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            token,
        )
        .unwrap();
        let local_config = git_local_config_file(dir).expect("resolve local config");
        write_session_config(
            token,
            "https://coord.example",
            "t",
            &[],
            Some(&local_config),
        )
        .unwrap();
    }

    /// The crash case: the session's teardown never ran, so the repo still
    /// names a token file the sweep is about to delete. The sweep must take
    /// the keys with it, or every non-interactive fetch there fails.
    #[test]
    fn sweep_unsets_helper_keys_recorded_in_a_stale_config() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let stale = temp.path().join("qontinui-git-cred-crashed-session.json");
        install_and_record(repo.path(), &stale);
        age_25h(&stale);

        let deleted = sweep_stale_cred_files(temp.path(), SWEEP_MAX_AGE, SystemTime::now());

        assert_eq!(deleted, 1);
        assert!(!stale.exists());
        assert_eq!(
            worktree_values(repo.path(), &helper_key()),
            vec![String::new()],
            "the helper pointing at the deleted token file must be unset (the reset stays)"
        );
        assert!(worktree_values(repo.path(), &use_http_path_key()).is_empty());
    }

    /// A linked worktree's install lands in ITS `config.worktree` (under the
    /// main checkout's `.git/worktrees/<id>/`), never in the shared config, and
    /// the worktree is usually deleted before the sweep runs. The recorded file
    /// must still be found and the keys removed from it.
    #[test]
    fn sweep_unsets_keys_a_deleted_linked_worktree_wrote_into_its_config_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let main = tempfile::tempdir().unwrap();
        let (_wt_parent, wt) = linked_worktree(main.path());

        let stale = temp.path().join("qontinui-git-cred-worktree-session.json");
        install_and_record(&wt, &stale);
        let recorded = git_local_config_file(&wt).unwrap();
        assert!(
            recorded.starts_with(main.path().join(".git").join("worktrees")),
            "precondition: the install is recorded in the worktree's own config.worktree, \
             got {}",
            recorded.display()
        );
        assert_eq!(
            worktree_values(&wt, &helper_key()),
            vec![String::new(), helper_value(&stale)],
            "precondition: the install lands in the worktree's own config.worktree"
        );
        assert!(
            shared_qontinui_helpers(main.path()).is_empty(),
            "precondition: the shared config carries no helper"
        );
        std::fs::remove_dir_all(&wt).unwrap();
        age_25h(&stale);

        sweep_stale_cred_files(temp.path(), SWEEP_MAX_AGE, SystemTime::now());

        assert_eq!(
            git_config_get_all_at(Some(&recorded), &helper_key()).unwrap(),
            vec![String::new()],
            "the helper must be unset from the gone worktree's config.worktree"
        );
        assert!(git_config_get_all_at(Some(&recorded), &use_http_path_key())
            .unwrap()
            .is_empty());
        assert!(!stale.exists());
    }

    /// A repo a LATER session installed into again names that session's live
    /// file, not the stale one. The sweep must leave both of its keys alone.
    #[test]
    fn sweep_leaves_a_repo_reinstalled_by_another_session() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let stale = temp.path().join("qontinui-git-cred-old-session.json");
        install_and_record(repo.path(), &stale);
        let live = temp.path().join("qontinui-git-cred-live-session.json");
        install_and_record(repo.path(), &live);
        age_25h(&stale);

        sweep_stale_cred_files(temp.path(), SWEEP_MAX_AGE, SystemTime::now());

        assert!(!stale.exists());
        assert!(live.exists());
        assert_eq!(
            worktree_values(repo.path(), &helper_key()),
            vec![String::new(), helper_value(&live)],
            "the live session's helper must survive"
        );
        assert_eq!(
            worktree_values(repo.path(), &use_http_path_key()),
            vec!["true".to_string()],
            "useHttpPath is still needed by the live install"
        );
    }

    /// A config file holding the helper value of a STALE token file next to a
    /// LIVE one: only the stale value goes, and `useHttpPath` stays because
    /// the live install still needs it.
    #[test]
    fn unset_orphaned_install_removes_only_the_stale_value_beside_a_live_one() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = temp.path().join("config");
        let stale = "C:/bin/qontinui-git-credential.exe --config C:/t/qontinui-git-cred-old.json";
        let live = "C:/bin/qontinui-git-credential.exe --config C:/t/qontinui-git-cred-new.json";
        git_config_add_at(Some(&cfg), "credential.helper", stale).unwrap();
        git_config_add_at(Some(&cfg), "credential.helper", live).unwrap();
        git_config_add_at(Some(&cfg), "credential.useHttpPath", "true").unwrap();

        // A name that is not a SUFFIX of any value must not match.
        assert_eq!(
            unset_orphaned_install(&cfg, Path::new("C:/t/qontinui-git-cred-ol")),
            OrphanUnset::NothingToDo
        );
        assert_eq!(
            unset_orphaned_install(&cfg, Path::new("C:/t/qontinui-git-cred-old.json")),
            OrphanUnset::Removed
        );

        assert_eq!(
            git_config_get_all_at(Some(&cfg), "credential.helper").unwrap(),
            vec![live.to_string()]
        );
        assert_eq!(
            git_config_get_all_at(Some(&cfg), "credential.useHttpPath").unwrap(),
            vec!["true".to_string()]
        );
    }

    /// The separator half of the git value-pattern: a value whose file name
    /// merely ENDS with the token name (no separator before it) must survive.
    /// Without the `[/\\]` both values would be removed.
    #[test]
    fn unset_orphaned_install_requires_a_path_separator_before_the_name() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = temp.path().join("config");
        let ours = "C:/bin/qontinui-git-credential.exe --config C:/t/qontinui-git-cred-old.json";
        let other = "C:/bin/qontinui-git-credential.exe --config C:/t/xqontinui-git-cred-old.json";
        git_config_add_at(Some(&cfg), "credential.helper", ours).unwrap();
        git_config_add_at(Some(&cfg), "credential.helper", other).unwrap();

        assert_eq!(
            unset_orphaned_install(&cfg, Path::new("C:/t/qontinui-git-cred-old.json")),
            OrphanUnset::Removed
        );
        assert_eq!(
            git_config_get_all_at(Some(&cfg), "credential.helper").unwrap(),
            vec![other.to_string()]
        );
    }

    /// A failed install (helper written, then the second key failed) must not
    /// leave the helper behind with its record deleted.
    #[test]
    fn discard_failed_install_removes_our_helper_before_the_token_file() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let token = temp.path().join("qontinui-git-cred-failed-install.json");
        let local_config = git_local_config_file(repo.path()).unwrap();
        let created = write_session_config(&token, "c", "t", &[], Some(&local_config)).unwrap();
        // The half-done install: only the helper key landed.
        git_config_add_at(Some(&local_config), &helper_key(), &helper_value(&token)).unwrap();

        discard_failed_install(
            "no-such-session-failed-install",
            &token,
            Some(&local_config),
            created,
        );

        assert!(!token.exists());
        assert!(worktree_values(repo.path(), &helper_key()).is_empty());
    }

    /// When another dir of the same session already installed successfully,
    /// a failed install must leave the shared token file alone.
    #[test]
    fn discard_failed_install_keeps_the_file_for_a_live_session() {
        let temp = tempfile::tempdir().unwrap();
        let token = temp.path().join("qontinui-git-cred-live-sibling.json");
        let session_id = format!(
            "test-discard-live-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        // A real, resolved config: only the registry check can keep the file.
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let local_config = git_local_config_file(repo.path()).unwrap();
        let created = write_session_config(&token, "c", "t", &[], Some(&local_config)).unwrap();
        registry()
            .lock()
            .unwrap()
            .entry(session_id.clone())
            .or_default()
            .dirs
            .push(PathBuf::from("C:/the/sibling/that/succeeded"));

        discard_failed_install(&session_id, &token, Some(&local_config), created);

        assert!(
            token.exists(),
            "the live sibling still needs the token file"
        );
        registry().lock().unwrap().remove(&session_id);
    }

    /// Another install of the session has recorded a DIFFERENT git config
    /// since this call created the file: the file is still that install's
    /// record, so a failed install must leave it.
    #[test]
    fn discard_failed_install_keeps_the_file_another_config_was_recorded_in() {
        let temp = tempfile::tempdir().unwrap();
        let token = temp.path().join("qontinui-git-cred-shared-record.json");
        let ours = temp.path().join("ours-config");
        let theirs = temp.path().join("theirs-config");
        let created = write_session_config(&token, "c", "t", &[], Some(&ours)).unwrap();
        write_session_config(&token, "c", "t", &[], Some(&theirs)).unwrap();

        discard_failed_install(
            "no-such-session-shared-record",
            &token,
            Some(&ours),
            created,
        );

        assert!(token.exists(), "another install's record must survive");
    }

    /// With no resolved git config there is no way to name where a helper
    /// value may have landed, so the token file is kept as the evidence.
    #[test]
    fn discard_failed_install_keeps_the_file_when_the_config_never_resolved() {
        let temp = tempfile::tempdir().unwrap();
        let token = temp.path().join("qontinui-git-cred-unresolved.json");
        let created = write_session_config(&token, "c", "t", &[], None).unwrap();

        discard_failed_install("no-such-session-unresolved", &token, None, created);

        assert!(token.exists());
    }

    /// The token file is the only record of where the keys are. When removing
    /// them fails (here: git cannot take `config.worktree.lock`), the sweep
    /// must keep it, and the next sweep must finish the job.
    #[test]
    fn sweep_keeps_the_token_file_when_a_recorded_removal_fails() {
        let temp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let stale = temp.path().join("qontinui-git-cred-locked-session.json");
        install_and_record(repo.path(), &stale);
        age_25h(&stale);
        let lock = config_lock_path(repo.path());
        std::fs::write(&lock, "").unwrap();

        let deleted = sweep_stale_cred_files(temp.path(), SWEEP_MAX_AGE, SystemTime::now());

        assert_eq!(deleted, 0);
        assert!(stale.exists(), "the record must survive a failed removal");
        assert!(worktree_values(repo.path(), &helper_key()).contains(&helper_value(&stale)));

        std::fs::remove_file(&lock).unwrap();
        let deleted = sweep_stale_cred_files(temp.path(), SWEEP_MAX_AGE, SystemTime::now());

        assert_eq!(deleted, 1);
        assert!(!stale.exists());
        assert_eq!(
            worktree_values(repo.path(), &helper_key()),
            vec![String::new()]
        );
        assert!(worktree_values(repo.path(), &use_http_path_key()).is_empty());
    }

    /// The lock git takes to write `dir`'s `config.worktree`.
    fn config_lock_path(dir: &Path) -> PathBuf {
        let file = git_local_config_file(dir).unwrap();
        PathBuf::from(format!("{}.lock", file.display()))
    }

    #[test]
    fn helper_names_token_file_is_anchored_to_a_path_separator() {
        let name = "qontinui-git-cred-a.json";
        assert!(helper_names_token_file(
            "x --config C:/t/qontinui-git-cred-a.json",
            name
        ));
        assert!(helper_names_token_file(
            r"x --config C:\t\qontinui-git-cred-a.json ",
            name
        ));
        assert!(!helper_names_token_file(
            "x --config C:/t/xqontinui-git-cred-a.json",
            name
        ));
        assert!(!helper_names_token_file(
            "x --config C:/t/qontinui-git-cred-a.json.bak",
            name
        ));
    }

    #[test]
    fn write_session_config_merges_git_configs_and_reports_creation() {
        let temp = tempfile::tempdir().unwrap();
        let token = temp.path().join("qontinui-git-cred-merge.json");
        let repos = vec!["qontinui/x".to_string()];

        let created =
            write_session_config(&token, "c", "t1", &repos, Some(Path::new("A"))).unwrap();
        assert!(created, "first write creates the file");
        let created =
            write_session_config(&token, "c", "t2", &repos, Some(Path::new("B"))).unwrap();
        assert!(!created, "a second dir of the same session does not");
        write_session_config(&token, "c", "t3", &repos, Some(Path::new("A"))).unwrap();
        write_session_config(&token, "c", "t4", &repos, None).unwrap();

        assert_eq!(recorded_git_configs(&token), vec!["A", "B"]);
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&token).unwrap()).unwrap();
        assert_eq!(config["push_token"], "t4");
        assert_eq!(config["repos"], json!(repos));

        // A file written before `git_configs` existed starts a fresh list.
        std::fs::write(&token, r#"{"push_token":"old"}"#).unwrap();
        write_session_config(&token, "c", "t5", &repos, Some(Path::new("C"))).unwrap();
        assert_eq!(recorded_git_configs(&token), vec!["C"]);
    }

    #[tokio::test]
    async fn refresh_tick_keeps_recorded_git_configs() {
        let temp = tempfile::tempdir().unwrap();
        let token = temp.path().join("qontinui-git-cred-refresh.json");
        write_session_config(&token, "c", "old", &[], Some(Path::new("A"))).unwrap();

        let outcome = refresh_tick(&token, || async { Ok("new".to_string()) })
            .await
            .unwrap();

        assert_eq!(outcome, TickOutcome::Refreshed);
        assert_eq!(recorded_git_configs(&token), vec!["A"]);
    }

    #[test]
    fn regex_literal_escapes_extended_regex_metacharacters() {
        assert_eq!(
            regex_literal("qontinui-git-cred-a.b+(c).json"),
            r"qontinui-git-cred-a\.b\+\(c\)\.json"
        );
    }

    #[test]
    fn cleanup_unsets_local_helper_and_removes_config() {
        let session_id = unique_session_id("cleanup");
        let config_path = config_file_path(&session_id);

        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        // Install exactly what `setup_credential_helper` writes, so the test
        // pins the install/teardown pair rather than just one key.
        install_and_record(repo.path(), &config_path);
        assert!(worktree_values(repo.path(), &helper_key()).contains(&helper_value(&config_path)));
        assert!(!worktree_values(repo.path(), &use_http_path_key()).is_empty());

        // Register the dir the way setup_credential_helper does, plus a
        // vanished dir cleanup must skip without erroring.
        {
            let mut reg = registry().lock().unwrap();
            let entry = reg.entry(session_id.clone()).or_default();
            entry.dirs.push(repo.path().to_path_buf());
            entry.dirs.push(PathBuf::from(
                "C:/definitely/not/a/real/dir/qontinui-test-gone",
            ));
        }

        cleanup_credential_helper(&session_id);

        // EVERY installed key must be gone. `useHttpPath` in particular: left
        // behind, it keeps changing credential lookup for this checkout long
        // after the session that installed it ended.
        assert_eq!(
            worktree_values(repo.path(), &helper_key()),
            vec![String::new()]
        );
        assert!(worktree_values(repo.path(), &use_http_path_key()).is_empty());
        assert!(!config_path.exists(), "config file should be removed");
        assert!(
            !registry().lock().unwrap().contains_key(&session_id),
            "registry entry should be drained"
        );
        // Idempotent: a second cleanup (double-close) is a no-op.
        cleanup_credential_helper(&session_id);
    }

    /// Two linked worktrees of one repo, one session each: each gets its OWN
    /// helper, the shared `.git/config` gets none, and closing one session
    /// leaves the other's helper intact. With `--local` both installs wrote
    /// the shared config and the second replaced the first.
    #[test]
    fn two_linked_worktrees_get_two_helpers_and_cleanup_of_one_keeps_the_other() {
        let first = unique_session_id("wt-first");
        let second = unique_session_id("wt-second");
        let main = tempfile::tempdir().unwrap();
        let (_wt1_parent, wt1) = linked_worktree(main.path());
        let (_wt2_parent, wt2) = add_linked_worktree(main.path());

        install_and_record(&wt1, &config_file_path(&first));
        assert!(
            shared_qontinui_helpers(main.path()).is_empty(),
            "the shared config carries no helper after the first install"
        );
        registry()
            .lock()
            .unwrap()
            .entry(first.clone())
            .or_default()
            .dirs
            .push(wt1.clone());
        install_and_record(&wt2, &config_file_path(&second));
        assert!(
            shared_qontinui_helpers(main.path()).is_empty(),
            "the shared config carries no helper after the second install"
        );

        assert_eq!(
            worktree_values(&wt1, &helper_key()),
            vec![String::new(), helper_value(&config_file_path(&first))]
        );
        assert_eq!(
            worktree_values(&wt2, &helper_key()),
            vec![String::new(), helper_value(&config_file_path(&second))]
        );
        assert_eq!(
            git_config_get(main.path(), "credential.helper"),
            None,
            "the main checkout reads no helper from the shared config"
        );

        cleanup_credential_helper(&first);

        assert_eq!(worktree_values(&wt1, &helper_key()), vec![String::new()]);
        assert_eq!(
            worktree_values(&wt2, &helper_key()),
            vec![String::new(), helper_value(&config_file_path(&second))],
            "the other worktree's helper must survive"
        );
        assert_eq!(
            worktree_values(&wt2, &use_http_path_key()),
            vec!["true".to_string()],
            "useHttpPath is still needed by the other install"
        );
        assert!(!config_file_path(&first).exists());

        cleanup_credential_helper(&second);
        assert!(!config_file_path(&second).exists());
    }

    /// A first install racing a sibling: the shared `config.lock` is held, so
    /// the write fails, but the sibling already set the key — success. With
    /// the key still unset the (retried) write fails.
    #[test]
    fn worktree_config_extension_write_tolerates_a_sibling_holding_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let lock = tmp.path().join(".git").join("config.lock");
        std::fs::write(&lock, "").unwrap();

        assert!(write_worktree_config_extension(tmp.path()).is_err());

        std::fs::remove_file(&lock).unwrap();
        let status = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "--local", "extensions.worktreeConfig", "true"])
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(&lock, "").unwrap();

        assert_eq!(write_worktree_config_extension(tmp.path()), Ok(()));
        std::fs::remove_file(&lock).unwrap();
    }

    /// The retry arm: the first write meets a held `config.lock` and the key
    /// is still unset; the lock is gone by the retry, which then succeeds.
    #[test]
    fn worktree_config_extension_write_retries_once_after_a_held_lock() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let lock = tmp.path().join(".git").join("config.lock");
        std::fs::write(&lock, "").unwrap();
        let releaser = {
            let lock = lock.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                std::fs::remove_file(&lock).unwrap();
            })
        };

        assert_eq!(write_worktree_config_extension(tmp.path()), Ok(()));
        releaser.join().unwrap();
        assert_eq!(
            git_config_get(tmp.path(), "extensions.worktreeConfig").as_deref(),
            Some("true")
        );
    }

    /// "Ours" is judged by the binary's file stem, not a substring of the
    /// value; a failed existence check cannot judge a path dead.
    #[test]
    fn dead_qontinui_helper_judges_ours_by_the_binary_stem() {
        // Built from a tempdir (never created) so it is absolute on every OS;
        // a POSIX literal is not `is_absolute()` on Windows.
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        let gone = gone.display();
        assert!(dead_qontinui_helper(&format!(
            "{gone}/qontinui-git-credential --config {gone}/qontinui-git-cred-a.json"
        ))
        .is_some());
        assert!(dead_qontinui_helper(&format!(
            "{gone}/qontinui-git-credential.exe --config {gone}/qontinui-git-cred-a.json"
        ))
        .is_some());
        assert!(
            dead_qontinui_helper(&format!(
                "{gone}/other-helper --config {gone}/qontinui-git-credential/qontinui-git-cred-a.json"
            ))
            .is_none(),
            "a value merely MENTIONING the stem is not ours"
        );
        assert!(!absolute_and_missing(Path::new(
            "relative/qontinui-git-credential"
        )));
    }

    /// From a linked worktree the shared config is the MAIN checkout's
    /// `.git/config` (resolved through git, since `.git` is a file there).
    #[test]
    fn git_shared_config_file_resolves_the_main_config_from_a_linked_worktree() {
        let main = tempfile::tempdir().unwrap();
        let (_wt_parent, wt) = linked_worktree(main.path());
        let shared = git_shared_config_file(&wt).expect("resolve the shared config");
        assert_eq!(
            std::fs::canonicalize(shared).unwrap(),
            std::fs::canonicalize(main.path().join(".git").join("config")).unwrap()
        );
    }

    /// Two installs under different keys (an agent-id one and a terminal
    /// session-id one) in ONE worktree: both helpers are listed, and cleaning
    /// up one leaves the other's value and its `useHttpPath`.
    #[test]
    fn two_installs_in_one_worktree_keep_each_others_helper() {
        let agent = unique_session_id("one-wt-agent");
        let terminal = unique_session_id("one-wt-terminal");
        let main = tempfile::tempdir().unwrap();
        let (_wt_parent, wt) = linked_worktree(main.path());

        install_and_record(&wt, &config_file_path(&agent));
        install_and_record(&wt, &config_file_path(&terminal));
        // A repeated install of the same key adds nothing.
        install_and_record(&wt, &config_file_path(&terminal));
        assert_eq!(
            worktree_values(&wt, &helper_key()),
            vec![
                String::new(),
                helper_value(&config_file_path(&agent)),
                helper_value(&config_file_path(&terminal)),
            ]
        );

        cleanup_credential_helper(&agent);

        assert_eq!(
            worktree_values(&wt, &helper_key()),
            vec![String::new(), helper_value(&config_file_path(&terminal))]
        );
        assert_eq!(
            worktree_values(&wt, &use_http_path_key()),
            vec!["true".to_string()]
        );

        cleanup_credential_helper(&terminal);

        assert_eq!(worktree_values(&wt, &helper_key()), vec![String::new()]);
        assert!(worktree_values(&wt, &use_http_path_key()).is_empty());
        assert!(shared_qontinui_helpers(main.path()).is_empty());
    }

    /// A dead runner's helper sits UNSCOPED in the shared config (the
    /// incident's shape). From a linked worktree with our install, a `git
    /// credential fill` for the coord host reaches ONLY our helper — the
    /// scoped reset in `config.worktree` clears both the global helper and the
    /// dead one — while a `github.com` fill still reaches the global helper.
    /// Isolated from the machine's config: `GIT_CONFIG_GLOBAL` points at a
    /// temp file, `GIT_CONFIG_NOSYSTEM` drops the system file.
    #[cfg(unix)]
    #[test]
    fn coord_fill_reaches_only_our_helper_past_a_dead_shared_one() {
        use std::os::unix::fs::PermissionsExt;

        let main = tempfile::tempdir().unwrap();
        let (_wt_parent, wt) = linked_worktree(main.path());
        let home = tempfile::tempdir().unwrap();

        let global = home.path().join("gitconfig");
        git_config_add_at(
            Some(&global),
            "credential.helper",
            "!f() { cat >/dev/null; echo username=global; echo password=global-pw; }; f",
        )
        .unwrap();

        let dead = "/nonexistent-qontinui-dead-runner/qontinui-git-credential \
                    --config /nonexistent-qontinui-dead-runner/qontinui-git-cred-dead.json";
        let status = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(main.path())
            .args(["config", "--local", "--add", "credential.helper", dead])
            .status()
            .unwrap();
        assert!(status.success());

        let bin_dir = home.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let ours = bin_dir.join("qontinui-git-credential");
        std::fs::write(
            &ours,
            "#!/bin/sh\ncat >/dev/null\nfor a; do op=$a; done\n\
             [ \"$op\" = get ] && { echo username=ours; echo password=ours-pw; }\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o755)).unwrap();
        let token = home.path().join("qontinui-git-cred-fill.json");
        set_git_credential_helper(&wt, TEST_SCOPE, &ours, &token).unwrap();
        assert!(
            worktree_values(&wt, &helper_key()).len() == 2,
            "our helper is in the worktree's own config.worktree"
        );
        assert_eq!(
            shared_qontinui_helpers(main.path()),
            vec![dead.to_string()],
            "nothing but the planted dead helper is in the shared config"
        );

        let fill = |host: &str| {
            use std::io::Write as _;
            let mut cmd = crate::process_helpers::no_window("git");
            cmd.arg("-C")
                .arg(&wt)
                .args(["credential", "fill"])
                .env("GIT_CONFIG_GLOBAL", &global)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", home.path())
                .env("GIT_TERMINAL_PROMPT", "0")
                .env_remove("GIT_ASKPASS")
                .env_remove("SSH_ASKPASS")
                .env_remove("GIT_CONFIG_COUNT")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = cmd.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(
                    format!("protocol=https\nhost={host}\npath=qontinui/x.git\n\n").as_bytes(),
                )
                .unwrap();
            let out = child.wait_with_output().unwrap();
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
            )
        };

        let (stdout, stderr) = fill("coord.example");
        assert!(
            stdout.contains("username=ours"),
            "coord fill must be answered by our helper: stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stderr.contains("nonexistent-qontinui-dead-runner"),
            "the dead shared helper must not be tried for the coord host: {stderr:?}"
        );

        let (stdout, stderr) = fill("github.com");
        assert!(
            stdout.contains("username=global"),
            "github.com fill must still reach the global helper: stdout={stdout:?} \
             stderr={stderr:?}"
        );
    }

    /// A worktree deleted before teardown runs leaves no dir to `git -C`
    /// into, but its keys sit in its `config.worktree` under the main
    /// checkout's `.git/worktrees/`. The recorded `git_configs` must reach
    /// them at teardown, not 24 h later.
    #[test]
    fn cleanup_unsets_keys_of_a_deleted_linked_worktree_from_the_record() {
        let session_id = unique_session_id("cleanup-gone-wt");
        let config_path = config_file_path(&session_id);
        let main = tempfile::tempdir().unwrap();
        let (_wt_parent, wt) = linked_worktree(main.path());

        install_and_record(&wt, &config_path);
        let recorded = git_local_config_file(&wt).unwrap();
        registry()
            .lock()
            .unwrap()
            .entry(session_id.clone())
            .or_default()
            .dirs
            .push(wt.clone());
        std::fs::remove_dir_all(&wt).unwrap();

        cleanup_credential_helper(&session_id);

        assert_eq!(
            git_config_get_all_at(Some(&recorded), &helper_key()).unwrap(),
            vec![String::new()],
            "the helper must be unset from the gone worktree's config at teardown"
        );
        assert!(git_config_get_all_at(Some(&recorded), &use_http_path_key())
            .unwrap()
            .is_empty());
        assert!(!config_path.exists());
    }

    /// A removal that fails at teardown (a contended `config.worktree.lock`)
    /// must keep the record without its token, so the boot sweep can retry and
    /// the helper serves nothing meanwhile.
    #[test]
    fn cleanup_leaves_a_tokenless_tombstone_when_a_removal_fails() {
        let session_id = unique_session_id("cleanup-locked");
        let config_path = config_file_path(&session_id);
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        install_and_record(repo.path(), &config_path);
        let local_config = git_local_config_file(repo.path()).unwrap();
        let lock = config_lock_path(repo.path());
        std::fs::write(&lock, "").unwrap();

        cleanup_credential_helper(&session_id);

        assert!(
            config_path.exists(),
            "the record must survive a failed removal"
        );
        assert!(is_tombstone(&config_path), "its token must be dropped");
        assert_eq!(
            recorded_git_configs(&config_path),
            vec![local_config.to_string_lossy().to_string()]
        );

        // The boot sweep retries once the lock is gone. Swept from a private
        // dir (same file name, which is all the helper value matches on) so
        // the test never sweeps the real %TEMP%.
        std::fs::remove_file(&lock).unwrap();
        let sweep_dir = tempfile::tempdir().unwrap();
        let moved = sweep_dir.path().join(config_path.file_name().unwrap());
        std::fs::copy(&config_path, &moved).unwrap();
        std::fs::remove_file(&config_path).unwrap();
        age_25h(&moved);
        assert_eq!(
            sweep_stale_cred_files(sweep_dir.path(), SWEEP_MAX_AGE, SystemTime::now()),
            1
        );
        assert_eq!(
            worktree_values(repo.path(), &helper_key()),
            vec![String::new()]
        );
        assert!(worktree_values(repo.path(), &use_http_path_key()).is_empty());
    }

    /// The tombstone a failed cleanup leaves has no token; the refresh loop
    /// must exit on it rather than mint a token for a closed session.
    #[tokio::test]
    async fn refresh_tick_exits_on_a_cleanup_tombstone() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("qontinui-git-cred-tombstone.json");
        std::fs::write(&config_path, json!({ "git_configs": ["A"] }).to_string()).unwrap();

        let fetched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fetched_flag = fetched.clone();
        let outcome = refresh_tick(&config_path, move || {
            fetched_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            async { Ok("fresh".to_string()) }
        })
        .await
        .unwrap();

        assert_eq!(outcome, TickOutcome::ConfigGone);
        assert!(
            !fetched.load(std::sync::atomic::Ordering::SeqCst),
            "no token may be minted for a closed session"
        );
        assert_eq!(recorded_git_configs(&config_path), vec!["A"]);
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(after.get("push_token").is_none(), "no token may be revived");
    }
}
