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
    // Delegates to the shared resolver; the dev-localhost guess is now logged
    // once per process via `coord_base_or_dev_localhost`.
    qontinui_runner_lib::profiles::coord_base_or_dev_localhost()
        .unwrap_or_else(|| "http://localhost:9870".to_string())
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
    std::process::Command::new("git")
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

fn set_git_credential_helper(
    working_dir: &Path,
    binary_path: &Path,
    config_path: &Path,
) -> Result<(), String> {
    let binary_str = binary_path.to_string_lossy().replace('\\', "/");
    let config_str = config_path.to_string_lossy().replace('\\', "/");
    let helper_value = format!("{binary_str} --config {config_str}");

    let output = std::process::Command::new("git")
        .args([
            "-C",
            &working_dir.to_string_lossy(),
            "config",
            "--local",
            "credential.helper",
            &helper_value,
        ])
        .output()
        .map_err(|e| format!("run git config: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git config --local credential.helper failed: {stderr}"
        ));
    }

    Ok(())
}

pub async fn setup_credential_helper(working_dir: &str, session_id: &str) {
    let working_path = Path::new(working_dir);
    if !is_git_repo(working_path) {
        debug!("credential_helper: {working_dir} is not a git repo, skipping");
        return;
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

pub fn cleanup_credential_helper(session_id: &str) {
    let config_path = config_file_path(session_id);
    if config_path.exists() {
        if let Err(e) = std::fs::remove_file(&config_path) {
            debug!("credential_helper: cleanup config file failed: {e}");
        }
    }
}
