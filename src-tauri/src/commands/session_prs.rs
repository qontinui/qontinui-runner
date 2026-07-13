//! Per-session PR status for the Terminal page's zone-header dropdown.
//!
//! RUNNER-LOCAL data source. Given a terminal's Claude `--session-id`, returns
//! every PR that session opened plus the merged/unmerged verdict,
//! unmerged-first. "Which PRs did session S open" is resolved entirely on this
//! device: every commit a session makes carries a `Session-Id: <session>` git
//! trailer, so the attribution reconciler ([`crate::session_pr_reconciler`])
//! walks each terminal's repo, matches head branches by that trailer, resolves
//! them to PRs via the GitHub API, and projects the result into
//! `project.session_prs`. This command just reads that projection — no coord
//! round-trip (coord's session→PR map is empty for interactive operator
//! sessions, which is what this feature exists to fix).
//!
//! FAIL-SOFT BY DESIGN: this feeds a passive status indicator that polls in
//! the background, so every degraded condition (PG unavailable, a query error,
//! the projection not yet populated) returns `Ok({ available: false, reason,
//! ... })` rather than `Err` — the dropdown hides itself instead of surfacing
//! an error toast per zone per poll. Only a malformed session id (a caller
//! bug) is a hard `Err`.

use serde_json::{json, Value};

use crate::database::pg::session_pr_ops::SessionPrRow;

/// Map the projection rows to the command's `available: true` envelope, in the
/// EXACT wire shape the frontend `useSessionPrs.ts` `SessionPr` interface reads
/// (`{repo, pr_number, branch, pr_state, merged, merged_at}`). `allMerged` is
/// true iff every row is merged (vacuously true for an empty set). Pure —
/// unit-tested below.
fn shape_available(rows: &[SessionPrRow]) -> Value {
    let prs: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "repo": r.repo,
                "pr_number": r.pr_number,
                "branch": r.head_branch,
                "pr_state": r.pr_state,
                "merged": r.merged,
                "merged_at": r.merged_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    let all_merged = rows.iter().all(|r| r.merged);
    json!({
        "available": true,
        "prs": prs,
        "allMerged": all_merged,
    })
}

/// The fail-soft envelope: a degraded condition hides the dropdown rather than
/// erroring. Pure.
fn fail_soft(reason: &str) -> Value {
    json!({
        "available": false,
        "reason": reason,
        "prs": [],
        "allMerged": true,
    })
}

/// `session_prs_get` — the session's authored PRs with merged verdicts,
/// unmerged-first (the query orders; the frontend renders verbatim).
///
/// Returns `{ available, reason?, prs: [{repo, pr_number, branch, pr_state,
/// merged, merged_at}], allMerged }`.
#[tauri::command]
pub async fn session_prs_get(claude_session_id: String) -> Result<Value, String> {
    let session_id = uuid::Uuid::parse_str(claude_session_id.trim())
        .map_err(|e| format!("invalid claude_session_id {claude_session_id:?}: {e}"))?;

    // Degraded boot with PG unreachable — hide the dropdown.
    if !crate::database::pg::pg_available() {
        return Ok(fail_soft("db_unavailable"));
    }
    let Some(pg_db) = crate::database::pg::PgDb::try_global() else {
        return Ok(fail_soft("db_unavailable"));
    };

    match pg_db.list_session_prs(session_id).await {
        Ok(rows) => Ok(shape_available(&rows)),
        Err(e) => {
            tracing::debug!("session_prs_get: list_session_prs failed (fail-soft): {e}");
            Ok(fail_soft("db_error"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn row(pr_number: i64, merged: bool) -> SessionPrRow {
        SessionPrRow {
            claude_session_id: uuid::Uuid::nil(),
            repo: "qontinui/qontinui-runner".to_string(),
            pr_number,
            head_branch: Some("feat/x".to_string()),
            pr_state: Some(if merged { "merged" } else { "open" }.to_string()),
            merged,
            merged_at: if merged {
                Some(Utc.with_ymd_and_hms(2026, 7, 13, 1, 2, 3).unwrap())
            } else {
                None
            },
        }
    }

    #[test]
    fn available_envelope_maps_rows_to_the_frontend_wire_shape() {
        let out = shape_available(&[row(7, false), row(4, true)]);
        assert_eq!(out["available"], json!(true));
        // Mixed merged state ⇒ not all merged.
        assert_eq!(out["allMerged"], json!(false));
        // Field names/casing are the exact `SessionPr` interface contract.
        assert_eq!(out["prs"][0]["pr_number"], json!(7));
        assert_eq!(out["prs"][0]["repo"], json!("qontinui/qontinui-runner"));
        assert_eq!(out["prs"][0]["branch"], json!("feat/x"));
        assert_eq!(out["prs"][0]["pr_state"], json!("open"));
        assert_eq!(out["prs"][0]["merged"], json!(false));
        assert_eq!(out["prs"][0]["merged_at"], Value::Null);
        // Merged row carries an RFC3339 merged_at string.
        assert_eq!(out["prs"][1]["merged"], json!(true));
        assert!(out["prs"][1]["merged_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-07-13"));
    }

    #[test]
    fn empty_projection_is_available_and_all_merged() {
        let out = shape_available(&[]);
        assert_eq!(out["available"], json!(true));
        assert_eq!(out["prs"], json!([]));
        // Vacuously all-merged (green) when the session has no PRs.
        assert_eq!(out["allMerged"], json!(true));
    }

    #[test]
    fn all_rows_merged_rolls_up_to_all_merged() {
        let out = shape_available(&[row(9, true), row(2, true)]);
        assert_eq!(out["allMerged"], json!(true));
    }

    #[test]
    fn fail_soft_hides_the_dropdown_without_erroring() {
        let out = fail_soft("db_unavailable");
        assert_eq!(out["available"], json!(false));
        assert_eq!(out["reason"], json!("db_unavailable"));
        assert_eq!(out["prs"], json!([]));
        // Fail-soft must never render red — allMerged stays true so the
        // trigger symbol is neutral/hidden, not a false "unmerged" alarm.
        assert_eq!(out["allMerged"], json!(true));
    }
}
