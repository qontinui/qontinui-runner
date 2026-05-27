use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::Emitter;
use tokio::sync::RwLock;
use tracing::{debug, warn};

static REGISTERED_REPOS_CACHE: once_cell::sync::Lazy<RwLock<(Instant, HashSet<String>)>> =
    once_cell::sync::Lazy::new(|| {
        RwLock::new((Instant::now() - Duration::from_secs(120), HashSet::new()))
    });

const CACHE_TTL: Duration = Duration::from_secs(60);

pub fn detect_repo_slug(working_dir: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", working_dir, "remote", "get-url", "origin"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_repo_slug(&url)
}

fn parse_repo_slug(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // SSH: git@github.com:owner/name.git
    if let Some(rest) = url.strip_prefix("git@") {
        let after_colon = rest.split_once(':')?.1;
        let slug = after_colon.trim_end_matches(".git");
        if slug.contains('/') && !slug.is_empty() {
            return Some(slug.to_string());
        }
    }

    // HTTPS: https://github.com/owner/name.git (or http)
    if url.starts_with("https://") || url.starts_with("http://") {
        if let Ok(parsed) = url::Url::parse(url) {
            let path = parsed
                .path()
                .trim_start_matches('/')
                .trim_end_matches(".git");
            let parts: Vec<&str> = path.splitn(3, '/').collect();
            if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                return Some(format!("{}/{}", parts[0], parts[1]));
            }
        }
    }

    None
}

fn coord_http_base() -> String {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(ws) = qontinui_runner_lib::profiles::load().coord_url.as_deref() {
        return crate::agent_worktree::coord_ws_to_http(ws);
    }
    "http://localhost:9870".to_string()
}

async fn fetch_registered_repos() -> Result<HashSet<String>, String> {
    let base = coord_http_base();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/canonical-repos");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "GET /coord/canonical-repos returned {}",
            resp.status().as_u16()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse canonical-repos body: {e}"))?;

    let repos = body
        .get("canonical_repos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("repo").and_then(|r| r.as_str()).map(String::from))
                .collect::<HashSet<String>>()
        })
        .unwrap_or_default();

    Ok(repos)
}

pub async fn is_repo_registered(slug: &str) -> bool {
    {
        let cache = REGISTERED_REPOS_CACHE.read().await;
        if cache.0.elapsed() < CACHE_TTL {
            return cache.1.contains(slug);
        }
    }

    match fetch_registered_repos().await {
        Ok(repos) => {
            let contains = repos.contains(slug);
            let mut cache = REGISTERED_REPOS_CACHE.write().await;
            *cache = (Instant::now(), repos);
            contains
        }
        Err(e) => {
            debug!("repo_detection: failed to fetch registered repos: {e}");
            false
        }
    }
}

pub async fn check_and_emit_unregistered(
    app_handle: tauri::AppHandle,
    working_dir: Option<String>,
) {
    let dir = match working_dir {
        Some(d) if !d.is_empty() => d,
        _ => return,
    };

    let slug = match tokio::task::spawn_blocking(move || detect_repo_slug(&dir)).await {
        Ok(Some(s)) => s,
        _ => return,
    };

    if is_repo_registered(&slug).await {
        return;
    }

    let payload = json!({ "repo": slug });
    if let Err(e) = app_handle.emit("repo-not-registered", &payload) {
        warn!("repo_detection: emit repo-not-registered failed: {e}");
    }
}

#[tauri::command]
pub async fn register_repo_with_coord(repo: String) -> Result<serde_json::Value, String> {
    let base = coord_http_base();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/canonical-repos");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = client
        .post(&url)
        .json(&json!({ "repo": repo }))
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;

    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read register body: {e}"))?;

    if !status.is_success() {
        return Err(format!(
            "POST /coord/canonical-repos returned {} — body: {body_text}",
            status.as_u16()
        ));
    }

    // Invalidate the cache so the next check sees the new registration.
    {
        let mut cache = REGISTERED_REPOS_CACHE.write().await;
        cache.0 = Instant::now() - Duration::from_secs(120);
    }

    serde_json::from_str::<serde_json::Value>(&body_text)
        .map_err(|e| format!("parse register body: {e} (raw: {body_text})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https_url() {
        assert_eq!(
            parse_repo_slug("https://github.com/acme/widget.git"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_https_url_no_git_suffix() {
        assert_eq!(
            parse_repo_slug("https://github.com/acme/widget"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_ssh_url() {
        assert_eq!(
            parse_repo_slug("git@github.com:acme/widget.git"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_ssh_url_no_git_suffix() {
        assert_eq!(
            parse_repo_slug("git@github.com:acme/widget"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_empty_url() {
        assert_eq!(parse_repo_slug(""), None);
    }

    #[test]
    fn parse_garbage() {
        assert_eq!(parse_repo_slug("not-a-url"), None);
    }
}
