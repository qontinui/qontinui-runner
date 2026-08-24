//! Runner-as-CI-node executor (plan `2026-07-15-runner-as-ci-node-migration`,
//! Phase 2).
//!
//! Coord dispatches candidate builds to connected runners over the SAME
//! Redis-fanout `/ws` socket family the agent runtime consumes:
//!
//! - inbound `events.ci.build_requested.<device_id>` — payload
//!   `{dispatch_id, repo, head_sha, fetch_url, candidate_ref, manifest_path,
//!   check_name, coord_http_url}`
//! - inbound `events.ci.build_cancelled.<device_id>` — payload `{dispatch_id}`
//! - inbound `events.ci.settings_requested.<device_id>` — the owner's
//!   CI-node configuration from qontinui-web, published by coord's
//!   `POST /devenv/ci-node-dispatch` and applied by [`settings_directive`]
//!   (which re-validates every field; coord is not this machine's security
//!   boundary)
//! - outbound REST, **device-JWT authenticated** (coord mounts the ingest
//!   routes behind `require_jwt` and matches the JWT's `device_id`/
//!   `tenant_id` claims against the dispatch row's assignee):
//!   `POST {coord}/coord/ci/dispatches/{id}/progress` (batched log lines +
//!   monotonic `progress_seq`) and `POST .../result` (`conclusion` +
//!   per-step `summary` + ~32 KB `log_tail` + the optional `test_results`
//!   JUnit artifact).
//!
//! That device-JWT binding is why the `test_results` artifact rides the
//! RESULT route rather than coord's `/coord/test-results/ingest`: the latter
//! is gated on `COORD_INGEST_TOKEN`, a fleet secret, and this lane runs on
//! customers' machines. The result route proves WHICH device is reporting for
//! WHICH dispatch, and coord already knows that dispatch's repo, sha and
//! tenant — so the artifact carries no attribution at all and a runner cannot
//! spoof another tenant's merge-gate inputs. See [`junit`].
//!
//! The executed commands come EXCLUSIVELY from the repo's own
//! `.qontinui/ci.toml` at the dispatched SHA (coord supplies no commands —
//! plan §4.1), gated by the local per-repo allowlist in
//! [`crate::settings::CiNodeSettings`]. Everything is off by default; with
//! `ci_node.enabled = false` this module admits nothing (it still listens,
//! so the owner can turn it on from the web — see [`settings_directive`]).
//!
//! Deliberately NOT on the local `:9876` surface: CI is driven only from the
//! coord WS (plan §7.6 — no new capability on the unauthenticated loopback
//! surface).

pub(crate) mod admission;
pub(crate) mod canonical;
pub(crate) mod checkout;
pub(crate) mod executor;
pub(crate) mod host_sizing;
pub(crate) mod junit;
pub(crate) mod manifest;
pub(crate) mod reporting;
pub(crate) mod services;
pub(crate) mod settings_directive;
pub(crate) mod sibling;
pub(crate) mod subscription;
pub(crate) mod tools;

use serde::Deserialize;
use tracing::info;

/// Default manifest path when the dispatch omits it.
fn default_manifest_path() -> String {
    ".qontinui/ci.toml".to_string()
}

/// A `events.ci.build_requested.<device_id>` dispatch payload. Unknown
/// fields are tolerated (coord may grow the shape); `manifest_path` and
/// `coord_http_url` degrade to sane defaults so a slightly-lean payload is
/// still runnable/reportable.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CiDispatchPayload {
    pub dispatch_id: String,
    /// Coord repo slug (`owner/name`) or bare repo name.
    pub repo: String,
    pub head_sha: String,
    /// Resolved fetch URL (for coord-origin repos this is the git door /
    /// mirror — a branch pushed only to GitHub is invisible there, memory
    /// `reference_coord_origin_branch_push_must_use_git_door`).
    pub fetch_url: String,
    /// `refs/heads/merge-candidate/<proposal_id>`.
    pub candidate_ref: String,
    /// Pull request this dispatch is validating, when there is one.
    ///
    /// This is the ONLY key the sibling declaration rule turns on
    /// ([`sibling::resolve_declaration`]), and its absence is a real answer,
    /// not a gap to paper over: coord pushes the SAME
    /// `refs/heads/merge-candidate/<proposal_id>` into every repo of a
    /// multi-repo proposal, so a dispatch with no pull request resolves
    /// siblings to their branch WITHOUT a declaration probe — exactly what
    /// the Actions composite action does off a `pull_request` event, and for
    /// the same reason (the candidate ref is force-pushed and deleted as the
    /// proposal resolves).
    ///
    /// Coord does not populate this today; until it does, every dispatch
    /// takes the branch path. Wiring it is a coord-side change to the
    /// `events.ci.build_requested.<device_id>` payload.
    #[serde(default)]
    pub pr_number: Option<u64>,
    #[serde(default = "default_manifest_path")]
    pub manifest_path: String,
    /// Check-run context coord will publish the verdict under. The runner
    /// only logs it (the verdict write is coord-side, keyed by dispatch_id).
    #[serde(default)]
    pub check_name: String,
    /// Coord HTTP base for the progress/result POSTs. Empty ⇒ fall back to
    /// the active profile's coord base.
    #[serde(default)]
    pub coord_http_url: String,
}

/// A `events.ci.build_cancelled.<device_id>` payload.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CiCancelPayload {
    pub dispatch_id: String,
}

/// A dispatch_id is used in filesystem paths (`.ci-worktrees/<id>`) and in
/// the progress/result URL path, so it must be a plain token. Coord mints
/// UUIDs; anything else is dropped before it can touch disk or a URL.
pub(crate) fn dispatch_id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The repo slug is joined into local paths via its basename; require plain
/// `owner/name`-style tokens so a hostile slug can't traverse.
pub(crate) fn repo_slug_is_safe(repo: &str) -> bool {
    !repo.is_empty()
        && repo.len() <= 200
        && !repo.contains("..")
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// Spawn the CI-node runtime. Mirrors `agent_runtime::spawn_runtime`:
/// no device identity or coord base ⇒ inert.
///
/// The subscription is held regardless of `ci_node.enabled` — the setting
/// gates ADMISSION, not delivery, because `settings_requested` is how the
/// opt-in gets flipped from qontinui-web and a disabled device holding no
/// socket could never receive it. See `subscription`'s module doc.
pub fn spawn_ci_node_runtime() {
    let Some(device_id) = crate::agent_runtime::load_local_device_id() else {
        info!(
            "ci_node: ~/.qontinui/machine.json missing or device_id unparseable — \
             CI-node runtime disabled. Skipping."
        );
        return;
    };
    if qontinui_runner_lib::profiles::connected_coord_base().is_none() {
        info!("ci_node: runner is ISOLATED (no coord configured, not a hosted tier) — CI-node runtime disabled. Skipping.");
        return;
    }
    info!("ci_node: starting for device_id={device_id}");
    tokio::spawn(async move {
        subscription::subscribe_loop(device_id).await;
    });
}

/// Cancel every in-flight CI build (app shutdown seam in `main.rs`). The
/// Windows Job Object (kill-on-close) is the hard backstop for the child
/// process trees; this token cancel lets executors attempt a best-effort
/// `cancelled` result POST before the process dies. Coord's dispatch-lease
/// sweeper covers whatever doesn't make it out.
pub fn shutdown_all() {
    admission::cancel_all_for_shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_id_safety_gate() {
        assert!(dispatch_id_is_safe("0198f2b4-1111-7aaa-bbbb-cccccccccccc"));
        assert!(dispatch_id_is_safe("abc_DEF-123"));
        assert!(!dispatch_id_is_safe(""));
        assert!(!dispatch_id_is_safe("../../etc"));
        assert!(!dispatch_id_is_safe("a/b"));
        assert!(!dispatch_id_is_safe("a b"));
        assert!(!dispatch_id_is_safe(&"x".repeat(65)));
    }

    #[test]
    fn repo_slug_safety_gate() {
        assert!(repo_slug_is_safe("qontinui/qontinui-runner"));
        assert!(repo_slug_is_safe("qontinui-runner"));
        assert!(repo_slug_is_safe("owner/repo.name"));
        assert!(!repo_slug_is_safe(""));
        assert!(!repo_slug_is_safe("owner/../secret"));
        assert!(!repo_slug_is_safe("repo name"));
        assert!(!repo_slug_is_safe("repo\\name"));
    }

    /// The dispatch payload parses from the pinned wire contract, and the
    /// two optional fields default when omitted.
    #[test]
    fn dispatch_payload_parses_pinned_contract() {
        let full: CiDispatchPayload = serde_json::from_value(serde_json::json!({
            "dispatch_id": "0198f2b4-1111-7aaa-bbbb-cccccccccccc",
            "repo": "qontinui/qontinui-runner",
            "head_sha": "deadbeef",
            "fetch_url": "https://github.com/qontinui/qontinui-runner.git",
            "candidate_ref": "refs/heads/merge-candidate/42",
            "manifest_path": ".qontinui/ci.toml",
            "check_name": "qontinui-ci-node",
            "coord_http_url": "https://coord.qontinui.io",
        }))
        .expect("pinned contract must parse");
        assert_eq!(full.manifest_path, ".qontinui/ci.toml");
        assert_eq!(full.check_name, "qontinui-ci-node");
        assert_eq!(full.pr_number, None);

        // The sibling declaration key, when coord grows it.
        let with_pr: CiDispatchPayload = serde_json::from_value(serde_json::json!({
            "dispatch_id": "d1",
            "repo": "r",
            "head_sha": "s",
            "fetch_url": "u",
            "candidate_ref": "refs/heads/merge-candidate/1",
            "pr_number": 1008,
        }))
        .expect("pr_number must parse");
        assert_eq!(with_pr.pr_number, Some(1008));

        let lean: CiDispatchPayload = serde_json::from_value(serde_json::json!({
            "dispatch_id": "d1",
            "repo": "r",
            "head_sha": "s",
            "fetch_url": "u",
            "candidate_ref": "refs/heads/merge-candidate/1",
        }))
        .expect("lean payload must parse with defaults");
        assert_eq!(lean.manifest_path, ".qontinui/ci.toml");
        assert!(lean.check_name.is_empty());
        assert!(lean.coord_http_url.is_empty());
    }
}
