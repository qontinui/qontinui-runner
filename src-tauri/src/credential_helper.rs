use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tracing::{debug, info, warn};

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

fn config_file_path(session_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("qontinui-git-cred-{session_id}.json"))
}

fn coord_http_base() -> String {
    // Delegates to the shared tier-aware policy fn: env → profile →
    // prod default on a hosted (qontinui_account-tier) runner → dev-localhost
    // guess (logged once per process) otherwise.
    qontinui_runner_lib::profiles::coord_base_with_source().0
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

fn is_git_repo(working_dir: &Path) -> bool {
    crate::process_helpers::no_window("git")
        .args([
            "-C",
            &working_dir.to_string_lossy(),
            "rev-parse",
            "--git-dir",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_config_local(working_dir: &Path, key: &str, value: &str) -> Result<(), String> {
    let output = crate::process_helpers::no_window("git")
        .args([
            "-C",
            &working_dir.to_string_lossy(),
            "config",
            "--local",
            key,
            value,
        ])
        .output()
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

    git_config_local(working_dir, "credential.helper", &helper_value)?;
    // Without useHttpPath git never sends `path=` to the helper, and the
    // helper's repo-registry lookup is keyed on the request path — the
    // install would be a production no-op.
    git_config_local(working_dir, "credential.useHttpPath", "true")?;

    Ok(())
}

/// Bound interactive pushes from this worktree: abort any HTTP transfer
/// trickling below 1 KiB/s for 60s. Without these a `git push` against
/// a genuinely stalled server hangs indefinitely (2026-07-12 incident:
/// coord git door 503-refused every push for ~5.5h). Best-effort, same
/// posture as the credential.helper write.
fn set_git_low_speed_bounds(working_dir: &Path) -> Result<(), String> {
    for (key, value) in [("http.lowSpeedLimit", "1024"), ("http.lowSpeedTime", "60")] {
        let output = crate::process_helpers::no_window("git")
            .args([
                "-C",
                &working_dir.to_string_lossy(),
                "config",
                "--local",
                key,
                value,
            ])
            .output()
            .map_err(|e| format!("run git config {key}: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git config --local {key} failed: {stderr}"));
        }
    }

    Ok(())
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

    let coord_base = coord_http_base();

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
    // protocol/host/path rewrite), so it needs just the token and the
    // registered-repo list for its emit decision — no coord_url.
    let config = json!({
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
/// Goal: no runner-spawned `git` op may ever reach Git Credential Manager's
/// blocking GUI popup — regardless of cwd. This covers the cases the
/// per-session `--local` coord helper (`setup_credential_helper`) never
/// touches: the non-repo umbrella root (`D:\qontinui-root`), unregistered
/// repos, and terminals that `cd` elsewhere. `GIT_TERMINAL_PROMPT=0` alone was
/// insufficient — it suppresses only the *terminal* prompt, not GCM's window.
///
/// The env set:
/// - `GCM_INTERACTIVE=never` — Git Credential Manager must never show its
///   GUI/interactive prompt; it fails non-interactively instead (returns a
///   stored credential if it has one, otherwise nothing — no window).
/// - `GIT_TERMINAL_PROMPT=0` — git never prompts for credentials on the
///   terminal either.
/// - a github.com-scoped `credential.helper` of `!gh auth git-credential`,
///   layered via git's `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`
///   env mechanism (NO file writes; applies to ALL cwds). This routes
///   unregistered / umbrella-root github access through the user's own `gh`
///   auth non-interactively instead of a popup.
///
/// PRECEDENCE — why this does NOT clobber the per-session coord helper for
/// REGISTERED repos: `credential.helper` is MULTI-VALUED. Git accumulates every
/// configured helper into an ordered list and queries them in config-read order
/// (system → global → local → worktree → `GIT_CONFIG_*` env) until one returns a
/// username+password. A repo's `--local` coord helper is therefore read — and
/// tried — BEFORE this env-injected helper: for a coord-registered repo the
/// coord helper emits the push token and git never reaches the `gh` fallback. We
/// deliberately do NOT reset the helper list (no empty-string entry): a reset
/// injected via env would be read after — and thus wipe — the local coord
/// helper, breaking registered-repo pushes.
///
/// For an UNREGISTERED repo / the umbrella root there is no local coord helper;
/// the machine default (`manager` = GCM) is tried first, but `GCM_INTERACTIVE=never`
/// turns its would-be popup into a silent non-interactive miss and git falls
/// through to `!gh auth git-credential`. If `gh` is absent that too fails
/// non-interactively (`GIT_TERMINAL_PROMPT=0`) — a clean error, never a window.
///
/// Set on the child env BEFORE any caller-supplied `extra_env` so a caller can
/// still intentionally override. Not platform-gated: GCM is Windows-centric but
/// every var here is harmless (and correct) cross-platform.
pub fn non_interactive_git_env() -> Vec<(String, String)> {
    vec![
        ("GCM_INTERACTIVE".to_string(), "never".to_string()),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        // One env-injected config pair: github.com-scoped credential.helper.
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        (
            "GIT_CONFIG_KEY_0".to_string(),
            "credential.https://github.com.helper".to_string(),
        ),
        (
            "GIT_CONFIG_VALUE_0".to_string(),
            "!gh auth git-credential".to_string(),
        ),
    ]
}

pub fn cleanup_credential_helper(session_id: &str) {
    let config_path = config_file_path(session_id);
    if config_path.exists() {
        if let Err(e) = std::fs::remove_file(&config_path) {
            debug!("credential_helper: cleanup config file failed: {e}");
        }
    }
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

    #[test]
    fn non_interactive_git_env_has_gcm_and_gh_fallback() {
        let env = non_interactive_git_env();
        let get = |k: &str| -> Option<&str> {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };

        // GCM must never pop its GUI; terminal prompts also off.
        assert_eq!(get("GCM_INTERACTIVE"), Some("never"));
        assert_eq!(get("GIT_TERMINAL_PROMPT"), Some("0"));

        // The github.com fallback helper is layered via GIT_CONFIG_* env so it
        // applies to every cwd with no file write. COUNT must match the number
        // of KEY_n/VALUE_n pairs actually present, or git ignores the block.
        let count: usize = get("GIT_CONFIG_COUNT")
            .expect("GIT_CONFIG_COUNT set")
            .parse()
            .expect("GIT_CONFIG_COUNT numeric");
        assert_eq!(count, 1, "exactly one env-injected config pair");
        for i in 0..count {
            assert!(
                get(&format!("GIT_CONFIG_KEY_{i}")).is_some(),
                "GIT_CONFIG_KEY_{i} present"
            );
            assert!(
                get(&format!("GIT_CONFIG_VALUE_{i}")).is_some(),
                "GIT_CONFIG_VALUE_{i} present"
            );
        }
        assert_eq!(
            get("GIT_CONFIG_KEY_0"),
            Some("credential.https://github.com.helper")
        );
        assert_eq!(get("GIT_CONFIG_VALUE_0"), Some("!gh auth git-credential"));

        // We must NOT inject an empty-string reset entry — a reset read from env
        // (after local config) would wipe the per-session --local coord helper
        // and break registered-repo pushes.
        assert!(
            !env.iter()
                .any(|(k, v)| k.starts_with("GIT_CONFIG_VALUE_") && v.is_empty()),
            "no empty-string credential.helper reset entry"
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
}
