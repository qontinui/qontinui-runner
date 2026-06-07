//! Runner-side **commit observation forwarder** — the push hook that feeds
//! coord's `POST /coord/commits/observations` ingest.
//!
//! Plan reference: `plans/commit-action-effect-signatures-plan.md` Phase 1.
//!
//! ## What it does
//!
//! Every `kind == "commit"` git-supervision event (recorded on the
//! [`super::SupervisionState`] ring) carries the libgit2-enriched commit
//! payload assembled in `trigger_system::watchers::git_watcher`
//! (`sha`, `message`, `author_name`, `author_email`, `timestamp`,
//! `parent_sha`, `changed_files`, plus the watcher's `repo_path` + branch
//! `detail`). This forwarder maps that payload to coord's single-commit
//! observation contract and POSTs it best-effort.
//!
//! ## Gating
//!
//! Behind env flag [`COMMIT_FORWARDER_FLAG_ENV`]
//! (`QONTINUI_COMMIT_FORWARDER_ENABLED`), **default off** — mirrors the
//! sibling Ξ_FS observer's `QONTINUI_FS_OBSERVER_ENABLED`. Off ⇒
//! [`spawn_forward_commit_event`] is a no-op.
//!
//! ## Degrade honestly
//!
//! The HTTP client shape mirrors `agent_worktree::fs_observer` (10s timeout,
//! best-effort, fire-and-forget on a spawned task). Coord being unreachable
//! is the expected degraded case (logged at `debug`); a non-2xx logs at
//! `warn`. Nothing here ever blocks or panics the ring / Tauri emit path.

use std::time::Duration;

use serde::Serialize;
use tracing::{debug, warn};

use super::GitSupervisionEvent;

/// Env flag that turns the commit forwarder on. Default off. Accepts the
/// usual truthy values; anything else (including unset) is false. Matches
/// the truthy-set used by [`crate::agent_worktree::fs_observer::fs_observer_enabled`].
pub const COMMIT_FORWARDER_FLAG_ENV: &str = "QONTINUI_COMMIT_FORWARDER_ENABLED";

/// Returns true iff the commit forwarder is enabled. Default off.
pub fn commit_forwarder_enabled() -> bool {
    matches!(
        std::env::var(COMMIT_FORWARDER_FLAG_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Body of `POST /coord/commits/observations` — a single commit observation
/// (NOT a batch). Field names are load-bearing: they are SHARED with coord's
/// ingest endpoint and must match exactly.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CommitObservation {
    pub agent_id: Option<String>,
    pub correlation_id: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub head_sha: String,
    pub parent_sha: Option<String>,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub changed_files: serde_json::Value,
    pub committed_at: Option<i64>,
    pub tenant_id: Option<String>,
}

/// Derive a repo basename (leaf dir name) from a watched repo path, e.g.
/// `D:/qontinui-root/qontinui-coord` ⇒ `qontinui-coord`. `None` when the
/// path is empty / has no leaf component.
fn repo_basename(repo_path: &str) -> Option<String> {
    let leaf = repo_path
        .replace('\\', "/")
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|s| s.to_string());
    leaf.filter(|s| !s.is_empty())
}

/// Build a [`CommitObservation`] from a `commit`-kind supervision event's
/// payload. Returns `None` when the payload lacks a usable `sha` (without a
/// head SHA there is nothing to record). `agent_id` / `correlation_id` /
/// `tenant_id` are `None` in Phase 1 — the watcher doesn't know them.
fn commit_observation_from_payload(payload: &serde_json::Value) -> Option<CommitObservation> {
    let head_sha = payload.get("sha").and_then(|v| v.as_str())?.to_string();
    if head_sha.is_empty() {
        return None;
    }

    let repo = payload
        .get("repo_path")
        .and_then(|v| v.as_str())
        .and_then(repo_basename);

    // The watcher's `detail` holds the branch name for a commit event
    // (extracted from `refs/heads/<branch>`). Treat the watcher's
    // detached-HEAD sentinels as null.
    let branch = payload
        .get("detail")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "unknown")
        .map(|s| s.to_string());

    let parent_sha = payload
        .get("parent_sha")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let author_name = payload
        .get("author_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let author_email = payload
        .get("author_email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let changed_files = payload
        .get("changed_files")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    let committed_at = payload.get("timestamp").and_then(|v| v.as_i64());

    Some(CommitObservation {
        agent_id: None,
        correlation_id: None,
        repo,
        branch,
        head_sha,
        parent_sha,
        message,
        author_name,
        author_email,
        changed_files,
        committed_at,
        tenant_id: None,
    })
}

/// POST a single [`CommitObservation`] to coord. Best-effort: any transport
/// error or non-2xx logs at `debug`/`warn` and returns — never surfaces a
/// coord failure to the caller.
async fn post_commit_observation(coord_http_base: &str, body: &CommitObservation) {
    let url = format!(
        "{}/coord/commits/observations",
        coord_http_base.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("commit_forwarder: build http client failed: {e}");
            return;
        }
    };
    // Attach the device-JWT bearer when one is available (never fatal —
    // unpaired / empty keychain collapses to the anonymous send coord accepts
    // today). Same write-path attach the Ξ_FS observer uses on its push, and it
    // feeds the data-plane auth-coverage metric. Must be `crate::auth`, not
    // `qontinui_runner_lib::auth` — this module compiles into the bin target,
    // and the lib path would bump the lib crate's separate counter statics,
    // invisible to the bin's `DATA_PLANE_TOTAL/AUTHED` coverage readout.
    let req = crate::auth::attach_device_auth(client.post(&url));
    match req.json(body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                debug!(
                    url = %url,
                    head_sha = %body.head_sha,
                    status = status.as_u16(),
                    "commit_forwarder: pushed commit observation"
                );
            } else {
                let txt = resp.text().await.unwrap_or_default();
                warn!(
                    url = %url,
                    status = status.as_u16(),
                    body = %txt,
                    "commit_forwarder: coord returned non-2xx (soft failure)"
                );
            }
        }
        Err(e) => {
            // Coord unreachable is the expected degraded case — debug, not warn.
            debug!(url = %url, error = %e, "commit_forwarder: push failed (coord unreachable)");
        }
    }
}

/// Top-level forwarder entry point: forward one `commit`-kind supervision
/// event to coord. Best-effort end to end; no-op when disabled, when the
/// event isn't a commit, when the payload has no usable SHA, or when no
/// coord base is configured.
///
/// `tokio::spawn`ed by [`spawn_forward_commit_event`] so the ring / Tauri
/// path is never delayed.
pub async fn forward_commit_event(event: &GitSupervisionEvent) {
    if !commit_forwarder_enabled() {
        return;
    }
    if event.kind != "commit" {
        return;
    }

    let body = match commit_observation_from_payload(&event.payload) {
        Some(b) => b,
        None => {
            debug!(
                id = %event.id,
                "commit_forwarder: payload missing sha — nothing to forward"
            );
            return;
        }
    };

    // Resolve the coord HTTP base the same machine-wide way the Ξ_FS census
    // / reclaim loops do (`COORD_HTTP_URL` env → active profile `coord_url`,
    // ws→http). `None` ⇒ no coord configured; cleanly skip. Fall back to the
    // public default so a configured-but-profileless runner still forwards.
    let coord_base = crate::agent_worktree::census::coord_http_base_pub()
        .or_else(|| std::env::var("QONTINUI_COORD_BASE_URL").ok())
        .unwrap_or_else(|| "https://coord.qontinui.io".to_string());

    post_commit_observation(&coord_base, &body).await;
}

/// Spawn the best-effort forward on the current Tokio runtime so the ring /
/// Tauri emit path is never delayed. Captures an owned clone so the future
/// is `'static`. A missing runtime is swallowed — the forwarder is optional.
///
/// Cheap-exits BEFORE cloning/spawning when the forwarder is disabled or the
/// event isn't a commit, so the non-commit path costs a single env read + a
/// string compare.
pub fn spawn_forward_commit_event(event: &GitSupervisionEvent) {
    if !commit_forwarder_enabled() {
        return;
    }
    if event.kind != "commit" {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        debug!("commit_forwarder: no tokio runtime — skipping forward");
        return;
    }
    let owned = event.clone();
    tokio::spawn(async move {
        forward_commit_event(&owned).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_payload() -> serde_json::Value {
        serde_json::json!({
            "event": "commit",
            "detail": "main",
            "repo_path": "D:/qontinui-root/qontinui-coord",
            "sha": "abc123",
            "message": "do a thing",
            "author_name": "Jane",
            "author_email": "jane@example.com",
            "timestamp": 1_700_000_000_i64,
            "parent_sha": "parent999",
            "changed_files": [{"path": "src/lib.rs", "status": "modified"}],
        })
    }

    #[test]
    fn enabled_is_false_by_default() {
        // Holds as long as no concurrent test set the flag (env is global).
        if std::env::var(COMMIT_FORWARDER_FLAG_ENV).is_err() {
            assert!(!commit_forwarder_enabled());
        }
    }

    #[test]
    fn repo_basename_strips_path() {
        assert_eq!(
            repo_basename("D:/qontinui-root/qontinui-coord").as_deref(),
            Some("qontinui-coord")
        );
        assert_eq!(
            repo_basename("D:\\qontinui-root\\qontinui-runner").as_deref(),
            Some("qontinui-runner")
        );
        assert_eq!(repo_basename("/repo/").as_deref(), Some("repo"));
        assert_eq!(repo_basename("").as_deref(), None);
    }

    #[test]
    fn maps_payload_to_contract() {
        let obs = commit_observation_from_payload(&commit_payload()).expect("some");
        assert_eq!(obs.head_sha, "abc123");
        assert_eq!(obs.parent_sha.as_deref(), Some("parent999"));
        assert_eq!(obs.repo.as_deref(), Some("qontinui-coord"));
        assert_eq!(obs.branch.as_deref(), Some("main"));
        assert_eq!(obs.message, "do a thing");
        assert_eq!(obs.author_name, "Jane");
        assert_eq!(obs.author_email, "jane@example.com");
        assert_eq!(obs.committed_at, Some(1_700_000_000));
        assert!(obs.agent_id.is_none());
        assert!(obs.correlation_id.is_none());
        assert!(obs.tenant_id.is_none());
        assert_eq!(obs.changed_files[0]["path"], "src/lib.rs");
    }

    #[test]
    fn root_commit_parent_sha_null_passes_through() {
        let mut p = commit_payload();
        p["parent_sha"] = serde_json::Value::Null;
        let obs = commit_observation_from_payload(&p).expect("some");
        assert!(obs.parent_sha.is_none());
    }

    #[test]
    fn detached_or_unknown_branch_is_null() {
        let mut p = commit_payload();
        p["detail"] = serde_json::json!("unknown");
        let obs = commit_observation_from_payload(&p).expect("some");
        assert!(obs.branch.is_none());
    }

    #[test]
    fn missing_sha_yields_none() {
        let mut p = commit_payload();
        p.as_object_mut().unwrap().remove("sha");
        assert!(commit_observation_from_payload(&p).is_none());
    }

    #[test]
    fn serializes_to_coord_contract() {
        let obs = commit_observation_from_payload(&commit_payload()).expect("some");
        let v = serde_json::to_value(&obs).expect("serialize");
        // agent_id / correlation_id / tenant_id present-and-null per contract.
        assert!(v["agent_id"].is_null());
        assert!(v["correlation_id"].is_null());
        assert!(v["tenant_id"].is_null());
        assert_eq!(v["repo"], "qontinui-coord");
        assert_eq!(v["branch"], "main");
        assert_eq!(v["head_sha"], "abc123");
        assert_eq!(v["parent_sha"], "parent999");
        assert_eq!(v["committed_at"], 1_700_000_000_i64);
        assert!(v["changed_files"].is_array());
    }
}
