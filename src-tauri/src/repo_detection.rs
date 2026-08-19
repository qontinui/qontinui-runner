use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::Emitter;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// `repo slug → owning tenant id` as coord reports it on
/// `GET /coord/canonical-repos`. The value is `None` for repos coord has
/// registered but not tenant-scoped (`canonical_repos.tenant_id IS NULL`,
/// the unscoped-pilot default), which is distinct from "repo absent" — the
/// KEY set is what `is_repo_registered` answers from.
type CanonicalRepos = HashMap<String, Option<String>>;

static REGISTERED_REPOS_CACHE: once_cell::sync::Lazy<RwLock<(Instant, CanonicalRepos)>> =
    once_cell::sync::Lazy::new(|| {
        RwLock::new((
            Instant::now() - Duration::from_secs(120),
            CanonicalRepos::new(),
        ))
    });

const CACHE_TTL: Duration = Duration::from_secs(60);

pub fn detect_repo_slug(working_dir: &str) -> Option<String> {
    let output = crate::process_helpers::no_window("git")
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

async fn fetch_registered_repos() -> Result<CanonicalRepos, String> {
    let base = qontinui_runner_lib::profiles::coord_base_with_source().0;
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/canonical-repos");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = crate::coord_http::coord_get(&client, &url)
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

    Ok(parse_canonical_repos(&body))
}

/// Project coord's `GET /coord/canonical-repos` body into `repo → tenant_id`.
/// Split out from the transport so the shape contract is unit-testable.
fn parse_canonical_repos(body: &serde_json::Value) -> CanonicalRepos {
    body.get("canonical_repos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let repo = item.get("repo").and_then(|r| r.as_str())?;
                    let tenant = item
                        .get("tenant_id")
                        .and_then(|t| t.as_str())
                        .map(String::from);
                    Some((repo.to_string(), tenant))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read the canonical-repo map, refreshing through coord when the cache is
/// cold. `Err` is reserved for a genuine coord failure so callers can tell
/// "coord said no tenant" from "coord didn't answer".
async fn canonical_repos() -> Result<CanonicalRepos, String> {
    {
        let cache = REGISTERED_REPOS_CACHE.read().await;
        if cache.0.elapsed() < CACHE_TTL {
            return Ok(cache.1.clone());
        }
    }
    let fresh = fetch_registered_repos().await?;
    let mut cache = REGISTERED_REPOS_CACHE.write().await;
    *cache = (Instant::now(), fresh.clone());
    Ok(fresh)
}

/// F2 — repo→tenant inference for the spawn picker's DEFAULT selection.
///
/// Accepts either an explicit `owner/name` slug or a `working_dir` to derive
/// one from (`git remote get-url origin`), so the caller can pass the active
/// tab's cwd without re-implementing slug parsing in TypeScript. `repo` wins
/// when both are supplied.
///
/// Returns the tenant id coord associates with the repo, or `None` when the
/// repo is unknown / not tenant-scoped / coord is unreachable. The frontend
/// treats `None` as "keep the active pin" and shows no error: inference is a
/// smart default, never a hard lock, so an unreachable coord must degrade
/// silently rather than block a spawn. That is also why coord failures return
/// `Ok(None)` rather than `Err`.
///
/// Source is coord's `canonical_repos.tenant_id` — the only repo→tenant view
/// a device JWT can read today. The authoritative ownership set lives in
/// `coord.tenant_repos`, which coord exposes only to Cognito-scoped operators
/// (`tenant_scope::TenantId` 403s a bare device JWT); when coord grows a
/// device-reachable `tenants_for_repo` route this fn should move to it.
#[tauri::command]
pub async fn tenant_for_repo(
    repo: Option<String>,
    working_dir: Option<String>,
) -> Result<Option<String>, String> {
    let explicit = repo
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let slug = match explicit {
        Some(s) => Some(s),
        None => match working_dir.filter(|d| !d.trim().is_empty()) {
            // `git remote get-url` shells out — keep it off the async runtime.
            Some(dir) => tokio::task::spawn_blocking(move || detect_repo_slug(&dir))
                .await
                .ok()
                .flatten(),
            None => None,
        },
    };

    let slug = match slug {
        Some(s) => s,
        None => return Ok(None),
    };

    match canonical_repos().await {
        Ok(map) => Ok(map.get(&slug).cloned().flatten()),
        Err(e) => {
            debug!("repo_detection: tenant_for_repo({slug}) failed: {e}");
            Ok(None)
        }
    }
}

pub async fn is_repo_registered(slug: &str) -> bool {
    match canonical_repos().await {
        Ok(repos) => repos.contains_key(slug),
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
    let base = qontinui_runner_lib::profiles::coord_base_with_source().0;
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/canonical-repos");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = crate::auth::attach_device_auth(client.post(&url).json(&json!({ "repo": repo })))
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
    fn canonical_repos_projects_repo_to_tenant() {
        let body = serde_json::json!({
            "canonical_repos": [
                { "repo": "acme/pizzeria", "tenant_id": "6b1f4b0e-0000-4000-8000-000000000001" },
                { "repo": "acme/unscoped", "tenant_id": serde_json::Value::Null },
                { "tenant_id": "6b1f4b0e-0000-4000-8000-000000000002" },
            ]
        });
        let map = parse_canonical_repos(&body);
        // Tenant-scoped repo resolves.
        assert_eq!(
            map.get("acme/pizzeria").cloned().flatten().as_deref(),
            Some("6b1f4b0e-0000-4000-8000-000000000001")
        );
        // Registered but unscoped: PRESENT as a key (so `is_repo_registered`
        // still says yes) with no tenant to infer.
        assert!(map.contains_key("acme/unscoped"));
        assert_eq!(map.get("acme/unscoped").cloned().flatten(), None);
        // An item with no `repo` is skipped entirely rather than keyed on "".
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn canonical_repos_tolerates_missing_or_malformed_body() {
        assert!(parse_canonical_repos(&serde_json::json!({})).is_empty());
        assert!(
            parse_canonical_repos(&serde_json::json!({ "canonical_repos": "nope" })).is_empty()
        );
    }

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
