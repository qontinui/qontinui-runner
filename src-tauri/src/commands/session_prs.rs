//! Per-session PR status for the Terminal page's zone-header dropdown.
//!
//! Proxies coord's `GET /pr-merge/session/:session_id/prs` (the inverse of
//! the PR→author-session resolver): given a terminal's Claude
//! `--session-id`, coord returns every PR that session's worktree
//! allocations produced plus the merged/unmerged verdict, unmerged-first.
//! The device JWT rides on the request via the shared
//! [`crate::coord_http::coord_get`] helper.
//!
//! FAIL-SOFT BY DESIGN: this feeds a passive status indicator that polls in
//! the background, so every degraded condition (unpaired device, coord
//! unreachable, endpoint not yet deployed, auth rejection) returns
//! `Ok({ available: false, reason, ... })` rather than `Err` — the dropdown
//! hides itself instead of surfacing an error toast per zone per poll. Only
//! a malformed session id (a caller bug) is a hard `Err`.

use std::time::Duration;

use serde_json::{json, Value};

/// Shape the command's fail-soft envelope from coord's HTTP verdict.
/// Pure — unit-tested below; the Tauri command is transport glue around it.
fn shape_response(status: u16, body: Value) -> Value {
    match status {
        200 => json!({
            "available": true,
            "prs": body.get("prs").cloned().unwrap_or_else(|| json!([])),
            "allMerged": body.get("all_merged").cloned().unwrap_or(json!(true)),
        }),
        401 | 403 => json!({
            "available": false,
            "reason": "unauthorized",
            "prs": [],
            "allMerged": true,
        }),
        // coord build predating the endpoint — the runner can land ahead of
        // coord; hide the dropdown until coord catches up.
        404 => json!({
            "available": false,
            "reason": "endpoint_missing",
            "prs": [],
            "allMerged": true,
        }),
        other => json!({
            "available": false,
            "reason": "coord_error",
            "status": other,
            "prs": [],
            "allMerged": true,
        }),
    }
}

/// `session_prs_get` — the session's authored PRs with merged verdicts,
/// unmerged-first (coord orders; the frontend renders verbatim).
///
/// Returns `{ available, reason?, prs: [{repo, pr_number, branch, pr_state,
/// merged, merged_at}], allMerged }`.
#[tauri::command]
pub async fn session_prs_get(claude_session_id: String) -> Result<Value, String> {
    let session_id = uuid::Uuid::parse_str(claude_session_id.trim())
        .map_err(|e| format!("invalid claude_session_id {claude_session_id:?}: {e}"))?;

    if !crate::coord_http::have_device_token() {
        return Ok(json!({
            "available": false,
            "reason": "unpaired",
            "prs": [],
            "allMerged": true,
        }));
    }

    let base = crate::coord_mcp::coord_base_url();
    let url = format!("{base}/pr-merge/session/{session_id}/prs");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = match crate::coord_http::coord_get(&client, &url).send().await {
        Ok(r) => r,
        Err(e) => {
            return Ok(json!({
                "available": false,
                "reason": "unreachable",
                "detail": e.to_string(),
                "prs": [],
                "allMerged": true,
            }));
        }
    };

    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    Ok(shape_response(status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_body_maps_prs_and_all_merged_through() {
        let out = shape_response(
            200,
            json!({
                "agent_session_id": "s",
                "prs": [{"repo": "o/r", "pr_number": 7, "merged": false}],
                "all_merged": false,
            }),
        );
        assert_eq!(out["available"], json!(true));
        assert_eq!(out["allMerged"], json!(false));
        assert_eq!(out["prs"][0]["pr_number"], json!(7));
    }

    #[test]
    fn missing_endpoint_and_auth_rejection_fail_soft() {
        let out = shape_response(404, Value::Null);
        assert_eq!(out["available"], json!(false));
        assert_eq!(out["reason"], json!("endpoint_missing"));
        assert_eq!(out["prs"], json!([]));

        let out = shape_response(403, Value::Null);
        assert_eq!(out["reason"], json!("unauthorized"));

        let out = shape_response(500, Value::Null);
        assert_eq!(out["reason"], json!("coord_error"));
        assert_eq!(out["status"], json!(500));
    }
}
