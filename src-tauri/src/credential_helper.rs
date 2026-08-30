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

/// The repo-local git config keys [`set_git_credential_helper`] installs —
/// and therefore exactly the keys [`cleanup_credential_helper`] must unset.
/// Kept as one list so the install and teardown sides can never drift: a key
/// added to the install without a matching unset would be left behind in the
/// working dir's `.git/config` forever (which is what happened to
/// `credential.useHttpPath`).
const INSTALLED_LOCAL_KEYS: [&str; 2] = ["credential.helper", "credential.useHttpPath"];

/// Per-session credential-helper state, keyed by the stringified coord
/// session UUID (the exact `session_id` passed to
/// [`setup_credential_helper`]).
#[derive(Default)]
struct SessionCredState {
    /// Working dirs where the repo-local [`INSTALLED_LOCAL_KEYS`] were
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
/// `terminal_create` / `spawn_worker_session`, and ~130 concurrent session
/// spawns were observed during the 2026-08-30 wedge — 130 unbounded
/// `.output()` calls behind one wedged git is 130 lost blocking-pool threads.
const GIT_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn is_git_repo(working_dir: &Path) -> bool {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args([
        "-C",
        &working_dir.to_string_lossy(),
        "rev-parse",
        "--git-dir",
    ]);
    matches!(
        crate::process_helpers::run_probe(
            cmd,
            GIT_CONFIG_TIMEOUT,
            "credential_helper: git rev-parse --git-dir"
        ),
        crate::process_helpers::ProbeOutcome::Captured(_)
    )
}

fn git_config_local(working_dir: &Path, key: &str, value: &str) -> Result<(), String> {
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
        .map_err(|e| format!("run git config: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git config --local {key} failed: {stderr}"));
    }

    Ok(())
}

fn set_git_credential_helper(
    working_dir: &Path,
    binary_path: &Path,
    config_path: &Path,
) -> Result<(), String> {
    let binary_str = binary_path.to_string_lossy().replace('\\', "/");
    let config_str = config_path.to_string_lossy().replace('\\', "/");
    let helper_value = format!("{binary_str} --config {config_str}");

    git_config_local(working_dir, INSTALLED_LOCAL_KEYS[0], &helper_value)?;
    // Without useHttpPath git never sends `path=` to the helper, and the
    // helper's repo-registry lookup is keyed on the request path — the
    // install would be a production no-op.
    git_config_local(working_dir, INSTALLED_LOCAL_KEYS[1], "true")?;

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

fn git_config_get_all_at(config_file: Option<&Path>, key: &str) -> Result<Vec<String>, String> {
    let mut cmd = git_config_cmd(config_file);
    cmd.args(["--get-all", key]);
    let output = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT)
        .map_err(|e| format!("run git config --get-all {key}: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut values: Vec<String> = stdout
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        // Drop the artifact of the final newline — but only one element, so a
        // genuinely empty-valued last entry ("key =") survives.
        if values.last().is_some_and(|l| l.is_empty()) {
            values.pop();
        }
        return Ok(values);
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
/// config precedence) still applies.
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
    if !is_git_repo(working_path) {
        debug!("credential_helper: {working_dir} is not a git repo, skipping");
        return;
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
    let config = json!({
        "coord_url": coord_base,
        "push_token": push_token,
        "repos": repos,
    });

    if let Err(e) = std::fs::write(&config_path, config.to_string()) {
        warn!("credential_helper: write config file failed: {e}");
        return;
    }

    match set_git_credential_helper(working_path, &binary_path, &config_path) {
        Ok(()) => {
            info!(
                session_id = session_id,
                config = %config_path.display(),
                "credential_helper: installed for {working_dir}"
            );
            // Best-effort global hygiene: keep Git Credential Manager (from
            // the system gitconfig) from popping an interactive password
            // prompt for the coord host. Never aborts setup.
            if let Err(e) = ensure_global_credential_hygiene(&coord_base, None) {
                warn!("credential_helper: global credential hygiene failed (non-fatal): {e}");
            }
            // Phase 4 — record the install in the in-process registry (so
            // session close can unset the repo-local config again) and
            // start the push-token refresh loop, deduped per session: a
            // second setup call for the SAME session registers the extra
            // working dir but must not spawn a second loop.
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
        }
        Err(e) => {
            warn!("credential_helper: git config failed: {e}");
            let _ = std::fs::remove_file(&config_path);
        }
    }
}

pub async fn setup_credential_helper_for_worktree(worktree_path: &Path, session_id: &str) {
    let dir_str = worktree_path.to_string_lossy().to_string();
    setup_credential_helper(&dir_str, session_id).await;
}

/// Non-interactive git credential posture injected into the environment of
/// EVERY process the runner spawns (interactive terminal PTYs + autonomous
/// direct-exec agents). Plan Phase 6 / P7.
///
/// Which hosts the non-interactive git posture applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitCredentialScope {
    /// Suppress ONLY github.com's GUI popup; leave every other host's
    /// interactive auth intact. For an interactive human TERMINAL, where a user
    /// may still legitimately want GCM to prompt for gitlab/azure/bitbucket.
    GithubOnly,
    /// Non-interactive for ALL hosts. For an AUTONOMOUS agent, which has no
    /// human to answer a GUI/terminal prompt — a popup there is an infinite
    /// hang, so a clean non-interactive failure is strictly better.
    AllHosts,
}

/// Env that gives a runner-spawned `git` a non-interactive GitHub credential
/// posture, so it never reaches Git Credential Manager's blocking GUI popup for
/// GitHub — covering the cases the per-session `--local` coord helper
/// (`setup_credential_helper`) never touches: the non-repo umbrella root
/// (`D:\qontinui-root`), unregistered repos, and terminals that `cd` elsewhere.
///
/// Always emitted (both scopes):
/// - a github.com-scoped `credential.helper` of `!gh auth git-credential`,
///   layered via git's `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`
///   env mechanism (NO file writes; applies to ALL cwds) — routes unregistered /
///   umbrella-root GitHub access through the user's own `gh` auth.
///
/// [`GitCredentialScope::GithubOnly`] additionally sets
/// `credential.https://github.com.interactive=false` (GCM honors URL-scoped
/// settings) so GCM does not pop its GUI for github.com specifically — WITHOUT
/// the global `GCM_INTERACTIVE`/`GIT_TERMINAL_PROMPT` that would also break a
/// human's first-time interactive auth to gitlab/azure/bitbucket in a terminal.
/// Worst case (an older GCM that ignores the scoped key) is the pre-fix status
/// quo for github.com — it can never REGRESS another host.
///
/// [`GitCredentialScope::AllHosts`] additionally sets `GCM_INTERACTIVE=never`
/// and `GIT_TERMINAL_PROMPT=0` globally — correct for an autonomous agent, which
/// must never block on ANY credential UI.
///
/// PRECEDENCE — why this does NOT clobber the per-session coord helper for
/// REGISTERED repos: `credential.helper` is MULTI-VALUED. Git accumulates every
/// configured helper into an ordered list and queries them in config-read order
/// (system → global → local → worktree → `GIT_CONFIG_*` env) until one returns a
/// username+password. A repo's `--local` coord helper is therefore read — and
/// tried — BEFORE this env-injected github.com helper: for a coord-registered
/// repo the coord helper emits the push token and git never reaches the `gh`
/// fallback. We deliberately do NOT reset the helper list (no empty-string
/// entry): a github.com-scoped reset injected via env is read AFTER — and would
/// thus wipe — the local coord helper, breaking registered-repo pushes.
///
/// The github.com config pair(s) are APPENDED after any `GIT_CONFIG_*` already
/// present in the child's inherited environment (we read `GIT_CONFIG_COUNT` and
/// index from there), so a caller/parent that already injected git config keeps
/// it rather than having `KEY_0`/`VALUE_0`/`COUNT` silently overwritten.
///
/// Set on the child env BEFORE any caller-supplied `extra_env` so a caller can
/// still intentionally override. Not platform-gated: GCM is Windows-centric but
/// every var here is harmless (and correct) cross-platform.
pub fn non_interactive_git_env(scope: GitCredentialScope) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();

    // The env-layered git config entries. Always the github.com gh helper;
    // GithubOnly adds the github.com-scoped GCM interactivity off-switch.
    let mut cfg: Vec<(&str, &str)> = vec![(
        "credential.https://github.com.helper",
        "!gh auth git-credential",
    )];
    match scope {
        GitCredentialScope::GithubOnly => {
            cfg.push(("credential.https://github.com.interactive", "false"));
        }
        GitCredentialScope::AllHosts => {
            out.push(("GCM_INTERACTIVE".to_string(), "never".to_string()));
            out.push(("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()));
        }
    }

    // Append to any inherited GIT_CONFIG_* rather than overwriting index 0.
    let base: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    for (i, (k, v)) in cfg.iter().enumerate() {
        let idx = base + i;
        out.push((format!("GIT_CONFIG_KEY_{idx}"), (*k).to_string()));
        out.push((format!("GIT_CONFIG_VALUE_{idx}"), (*v).to_string()));
    }
    out.push((
        "GIT_CONFIG_COUNT".to_string(),
        (base + cfg.len()).to_string(),
    ));
    out
}

/// Outcome of one refresh-loop tick. Factored into a type (rather than a
/// bare bool) so the loop's exit signal is explicit.
#[derive(Debug, PartialEq, Eq)]
enum TickOutcome {
    /// Config rewritten with a fresh token.
    Refreshed,
    /// Config file no longer exists (cleanup or the startup sweep removed
    /// it) — the loop must exit.
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
    if !config_path.exists() {
        return Ok(TickOutcome::ConfigGone);
    }
    let fresh_token = fetch().await?;
    let raw = match std::fs::read_to_string(config_path) {
        Ok(raw) => raw,
        // Raced with cleanup between the exists() check and the read — same
        // exit signal as the up-front check.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TickOutcome::ConfigGone),
        Err(e) => return Err(format!("read {}: {e}", config_path.display())),
    };
    let mut config: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse config: {e}"))?;
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

/// Tear down everything [`setup_credential_helper`] installed for a session:
/// unset every repo-local key in [`INSTALLED_LOCAL_KEYS`] in each working dir
/// the session registered, then remove the token config file (whose absence
/// is also the refresh loop's exit signal). Idempotent and best-effort —
/// every failure is debug!-logged, never propagated, because this runs inside
/// teardown funnels which must not fail on credential hygiene.
///
/// `session_id` is whatever key the matching `setup_credential_helper` call
/// used. Two disjoint key spaces exist and BOTH must be torn down:
/// - the coord **session** UUID (terminal / worker installs), drained by
///   `SessionRegistry::close`;
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

    for dir in dirs {
        if !dir.exists() {
            debug!(
                "credential_helper: cleanup skipping missing dir {}",
                dir.display()
            );
            continue;
        }
        // Unset EVERY key the install wrote. Leaving `credential.useHttpPath`
        // behind is not cosmetic: it permanently changes credential lookup for
        // that repo, so the next helper in the chain (GCM) starts keying its
        // store by full path and re-prompts per path — the very prompt class
        // this subsystem exists to eliminate.
        for key in INSTALLED_LOCAL_KEYS {
            let mut cmd = crate::process_helpers::no_window("git");
            cmd.args([
                "-C",
                &dir.to_string_lossy(),
                "config",
                "--local",
                "--unset-all",
                key,
            ]);
            let output = crate::process_helpers::output_with_timeout(cmd, GIT_CONFIG_TIMEOUT);
            match output {
                // Exit code 5 = key not present — the desired end state already
                // holds (e.g. the operator hand-cleaned the repo).
                Ok(out) if out.status.success() || out.status.code() == Some(5) => {}
                Ok(out) => debug!(
                    "credential_helper: unset {key} in {} failed ({:?}): {}",
                    dir.display(),
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                ),
                Err(e) => debug!(
                    "credential_helper: run git config --unset-all {key} in {}: {e}",
                    dir.display()
                ),
            }
        }
    }

    let config_path = config_file_path(session_id);
    if config_path.exists() {
        if let Err(e) = std::fs::remove_file(&config_path) {
            debug!("credential_helper: cleanup config file failed: {e}");
        }
    }
}

/// [`cleanup_credential_helper`] on a detached thread — the form safe to call
/// from a `Drop` impl.
///
/// Two reasons the direct call is wrong there. (1) Cleanup shells out to
/// `git config` once per installed key per dir; running that inline blocks
/// whichever thread the drop lands on, which for `IsolatedEditContext` is a
/// Tokio worker. (2) Cleanup can panic on a poisoned registry mutex, and a
/// panic raised while unwinding another panic aborts the process — the same
/// hazard `IsolatedEditContext::drop` already guards its claim-release spawn
/// against. A detached `std::thread` needs no runtime (unlike `tokio::spawn`,
/// which itself panics outside one) and contains any panic to itself.
pub fn spawn_cleanup_credential_helper(session_id: String) {
    std::thread::spawn(move || cleanup_credential_helper(&session_id));
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
        match std::fs::remove_file(entry.path()) {
            Ok(()) => deleted += 1,
            Err(e) => debug!("credential_helper: sweep remove {name}: {e}"),
        }
    }
    deleted
}

/// One-shot, best-effort boot sweep of stale credential config files in
/// %TEMP% (files older than [`SWEEP_MAX_AGE`]; younger ones may belong to a
/// live session — possibly another runner instance sharing this temp dir —
/// and are left alone). Runs on a detached thread so it can never slow or
/// fail the boot; errors never propagate (debug!-logged inside the sweep).
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
    use super::*;

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

    #[test]
    fn set_git_credential_helper_writes_helper_and_use_http_path() {
        let tmp = tempfile::tempdir().unwrap();
        let status = crate::process_helpers::no_window("git")
            .args(["init", "-q", &tmp.path().to_string_lossy()])
            .status()
            .expect("git init");
        assert!(status.success());

        set_git_credential_helper(
            tmp.path(),
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            Path::new("C:\\tmp\\cred.json"),
        )
        .expect("set_git_credential_helper");

        assert_eq!(
            git_config_get(tmp.path(), "credential.helper").as_deref(),
            Some("C:/bin/qontinui-git-credential.exe --config C:/tmp/cred.json")
        );
        // Without useHttpPath git never passes `path=` to the helper, which
        // makes the whole install a production no-op — it must be set.
        assert_eq!(
            git_config_get(tmp.path(), "credential.useHttpPath").as_deref(),
            Some("true")
        );
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
    fn non_interactive_git_env_github_only_scopes_to_github_leaves_other_hosts() {
        let env = non_interactive_git_env(GitCredentialScope::GithubOnly);
        let has = |k: &str| env.iter().any(|(key, _)| key == k);

        // GithubOnly must NOT set the GLOBAL non-interactive vars — a human
        // terminal keeps interactive auth for gitlab/azure/bitbucket.
        assert!(!has("GCM_INTERACTIVE"), "no global GCM_INTERACTIVE");
        assert!(!has("GIT_TERMINAL_PROMPT"), "no global GIT_TERMINAL_PROMPT");

        let cfg = injected_git_config(&env);
        assert!(cfg.contains(&(
            "credential.https://github.com.helper".to_string(),
            "!gh auth git-credential".to_string()
        )));
        // GCM off for github.com specifically (not globally).
        assert!(cfg.contains(&(
            "credential.https://github.com.interactive".to_string(),
            "false".to_string()
        )));
        assert!(
            !env.iter()
                .any(|(k, v)| k.starts_with("GIT_CONFIG_VALUE_") && v.is_empty()),
            "no empty-string reset entry (would wipe the --local coord helper)"
        );
    }

    #[test]
    fn non_interactive_git_env_all_hosts_is_fully_non_interactive() {
        let env = non_interactive_git_env(GitCredentialScope::AllHosts);
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };

        // AllHosts (autonomous agent) never blocks on any host's UI.
        assert_eq!(get("GCM_INTERACTIVE"), Some("never"));
        assert_eq!(get("GIT_TERMINAL_PROMPT"), Some("0"));

        let cfg = injected_git_config(&env);
        assert!(cfg.contains(&(
            "credential.https://github.com.helper".to_string(),
            "!gh auth git-credential".to_string()
        )));
        assert!(
            !env.iter()
                .any(|(k, v)| k.starts_with("GIT_CONFIG_VALUE_") && v.is_empty()),
            "no empty-string reset entry"
        );
    }

    #[test]
    fn set_git_credential_helper_fails_outside_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let err = set_git_credential_helper(
            tmp.path(),
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            Path::new("C:\\tmp\\cred.json"),
        )
        .unwrap_err();
        assert!(err.contains("credential.helper"), "unexpected error: {err}");
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

    #[test]
    fn cleanup_unsets_local_helper_and_removes_config() {
        // Unique per-run session id: cleanup resolves the config file in the
        // real %TEMP% (production path), so the name must not collide with
        // parallel test runs.
        let session_id = format!(
            "test-cleanup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let repo = tempfile::tempdir().unwrap();
        let status = crate::process_helpers::no_window("git")
            .args(["init", "-q", &repo.path().to_string_lossy()])
            .status()
            .expect("git init");
        assert!(status.success());
        // Install exactly what `set_git_credential_helper` writes, so the test
        // pins the install/teardown pair rather than just one key.
        set_git_credential_helper(
            repo.path(),
            Path::new("C:\\bin\\qontinui-git-credential.exe"),
            Path::new("C:\\tmp\\cred.json"),
        )
        .unwrap();
        for key in INSTALLED_LOCAL_KEYS {
            assert!(
                git_config_get(repo.path(), key).is_some(),
                "{key} should be installed"
            );
        }

        let config_path = config_file_path(&session_id);
        std::fs::write(&config_path, "{}").unwrap();

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

        // EVERY installed key must be gone. `credential.useHttpPath` in
        // particular: left behind, it keeps changing credential lookup for
        // this repo long after the session that installed it ended.
        for key in INSTALLED_LOCAL_KEYS {
            assert_eq!(
                git_config_get(repo.path(), key),
                None,
                "local {key} should be unset"
            );
        }
        assert!(!config_path.exists(), "config file should be removed");
        assert!(
            !registry().lock().unwrap().contains_key(&session_id),
            "registry entry should be drained"
        );
        // Idempotent: a second cleanup (double-close) is a no-op.
        cleanup_credential_helper(&session_id);
    }
}
