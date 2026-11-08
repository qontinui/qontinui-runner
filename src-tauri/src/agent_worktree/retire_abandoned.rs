//! Retire the rows of an allocation the runner abandoned before materializing
//! it (plan
//! `2026-10-31-continuation-worktree-acquire-failure-strands-the-allocation-and-tells-the-session-nothing`
//! Phase 3).
//!
//! Once coord has answered `/agents/allocate` with rows, every later unwind —
//! a claim that could not be taken, a refused shared-branch placement, a
//! failed `git worktree add` — used to leave those rows `allocated` with no
//! directory, so every overlap read saw a live agent until session-gone
//! reclaim aged them out. Each such row is now handed back through coord's
//! own retire door, `POST /coord/worktree-done/:device_id` with
//! `abandoned: true`, sending `worktree_path` exactly as coord returned it.
//!
//! The retire is detached: it never delays the caller's fallback and never
//! changes the error it returns. It is also patient, because the commonest
//! reason to be here is that coord's claim door was failing, so an immediate
//! single POST would most likely fail for the very rows that need it. The
//! first attempt is immediate; later ones follow the acquire's delay helper
//! starting at 30 s, capped at 5 min, inside a 30 min budget. A typed answer
//! (retired, refused, no row, an unreadable answer, a coord that does not
//! implement the flag) ends
//! the loop. A runner exit drops the task and the row ages out as before.
//!
//! No directory is removed here: `abandoned` grants no disk authority, and the
//! existing unwind only releases claims. Disk stays with reclaim.
//!
//! Out of scope: coord's dispatch-time rows (`payload.allocation`,
//! `dispatch_source` set). This module only ever sees the rows of the runner's
//! own allocate response.

use std::time::Duration;

use tracing::{info, warn};

use super::acquire_retry::{self, Attempt, RetryDeadline};
use super::{AllocateError, CoordAllocatedWorktree};

/// Total time the retire keeps trying.
const RETIRE_BUDGET: Duration = Duration::from_secs(30 * 60);
const RETIRE_BASE_DELAY_SECS: u64 = 30;
const RETIRE_CAP_DELAY_SECS: u64 = 5 * 60;
/// Each retire POST is bounded; a hung send must not pin the task.
const RETIRE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

/// One coord row to hand back, exactly as coord returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetireRow {
    pub(crate) repo: String,
    pub(crate) worktree_path: String,
    pub(crate) branch: String,
}

/// Every row of coord's allocate response, captured before anything consumes
/// it. Declared siblings included: coord minted them for this allocation too.
pub(super) fn rows_to_retire(rows: &[CoordAllocatedWorktree]) -> Vec<RetireRow> {
    rows.iter()
        .map(|w| RetireRow {
            repo: w.repo.clone(),
            worktree_path: w.worktree_path.clone(),
            branch: w.branch.clone(),
        })
        .collect()
}

/// The retire door's URL for this device.
pub(crate) fn retire_url(coord_http_base: &str, device_id: uuid::Uuid) -> String {
    format!(
        "{}/coord/worktree-done/{device_id}",
        coord_http_base.trim_end_matches('/')
    )
}

/// The request body: coord's own `worktree_path` and `branch`, plus
/// `abandoned: true` — the same shape `allocate-worktree.sh --done --abandon`
/// sends.
pub(crate) fn retire_body(row: &RetireRow) -> serde_json::Value {
    serde_json::json!({
        "worktree_path": row.worktree_path,
        "branch": row.branch,
        "abandoned": true,
    })
}

/// What coord said about one retire.
///
/// Coord's `worktree-done` answer has no `status` field. It carries
/// `abandoned`, the retire UPDATE's own row count, and `abandon_refused`, why
/// that count is not higher; the two are set independently. `persisted:
/// false` is its error arm (nothing applied, `matched` 0 there is not a
/// no-match). Read `post_worktree_done` in coord's `worktree_reclaim.rs` and
/// `AgentDoneOutcome` in its `agent_worktrees.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetireOutcome {
    /// HTTP 200 with `abandoned` above 0. A shortfall reason beside it
    /// (a partial retire across several matched rows) is kept for the log.
    Retired { shortfall: Option<String> },
    /// HTTP 200 with `abandoned: 0` and coord's reason (`no_retirable_row`,
    /// `census_silent_for_device`, a sighting inside the window, …).
    Refused(String),
    /// HTTP 200 with no `abandoned` count and no reason: a coord that
    /// predates the flag.
    NotSupported,
    /// HTTP 404: coord holds no such row (already retired, or never written).
    NoRow,
    /// Any other 4xx: a final answer.
    Rejected(String),
    /// HTTP 200 whose body could not be read as coord's answer. What coord did
    /// is UNKNOWN; re-sending a fully read body would only read it again.
    Unreadable(String),
}

/// Whether a refusal is coord failing to decide rather than a judgement about
/// the row. `retire_failed_closed` names three things in coord's
/// `stamp_agent_done_and_retire`: the census-presence probe erroring (Redis),
/// the census-gated UPDATE erroring (Postgres), and a misconfigured census
/// window. In the first two coord moved nothing because it was failing, which
/// is the outage this loop exists to ride out. A misconfigured window is a
/// configuration no retry inside the budget changes.
fn coord_failed_to_decide(reason: &str) -> bool {
    reason.starts_with("retire_failed_closed:") && !reason.contains("census presence window")
}

/// Classify one fully read retire response. `Err` is a transient answer worth
/// another attempt: a 5xx, coord's `persisted: false` error arm, or a
/// `retire_failed_closed` refusal coord made because its own store failed.
/// Pure, so the vocabulary is testable.
pub(crate) fn classify_retire_response(
    status: reqwest::StatusCode,
    body_text: &str,
) -> Result<RetireOutcome, String> {
    if acquire_retry::status_is_transient(status) {
        return Err(format!(
            "worktree-done returned {} — body: {}",
            status.as_u16(),
            one_line(body_text)
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(RetireOutcome::NoRow);
    }
    if !status.is_success() {
        return Ok(RetireOutcome::Rejected(format!(
            "HTTP {} — body: {}",
            status.as_u16(),
            one_line(body_text)
        )));
    }
    let v: serde_json::Value = match serde_json::from_str(body_text) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        _ => return Ok(RetireOutcome::Unreadable(one_line(body_text))),
    };
    if v.get("persisted").and_then(|p| p.as_bool()) == Some(false) {
        return Err(format!(
            "coord did not apply the retire (persisted:false): {}",
            one_line(v.get("note").and_then(|n| n.as_str()).unwrap_or(""))
        ));
    }
    let refused = match v.get("abandon_refused") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    };
    if let Some(reason) = refused.as_deref() {
        if coord_failed_to_decide(reason) {
            return Err(format!("coord could not decide: {}", one_line(reason)));
        }
    }
    let abandoned = match v.get("abandoned") {
        None | Some(serde_json::Value::Null) => None,
        Some(n) => match n.as_u64() {
            Some(n) => Some(n),
            None => return Ok(RetireOutcome::Unreadable(one_line(body_text))),
        },
    };
    Ok(match (abandoned, refused) {
        (Some(n), shortfall) if n > 0 => RetireOutcome::Retired {
            shortfall: shortfall.map(|r| one_line(&r)),
        },
        (Some(_), Some(reason)) => RetireOutcome::Refused(one_line(&reason)),
        (Some(_), None) => {
            RetireOutcome::Refused("coord moved no row and named no reason".to_string())
        }
        // A reason with no count is not a coord without the flag: keep it.
        (None, Some(reason)) => RetireOutcome::Refused(one_line(&reason)),
        (None, None) => RetireOutcome::NotSupported,
    })
}

/// Bound a remote body to one short log line.
fn one_line(s: &str) -> String {
    const MAX: usize = 300;
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX)
        .collect();
    if s.chars().count() > MAX {
        format!("{flat}…")
    } else {
        flat
    }
}

async fn retire_once(url: &str, body: &serde_json::Value) -> Attempt<RetireOutcome, String> {
    let client = crate::coord_http::coord_client()
        .cloned()
        .unwrap_or_else(reqwest::Client::new);
    // The retire door is keyed by device alone, which is what `Device` scope
    // presents.
    let resp = match crate::auth::attach_device_auth_for(
        client.post(url).timeout(RETIRE_ATTEMPT_TIMEOUT).json(body),
        crate::auth::TenantScope::Device,
    )
    .send()
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return Attempt::Transient(format!(
                "POST {url}: {}",
                acquire_retry::describe_reqwest_error(&e)
            ))
        }
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return Attempt::Transient(format!(
                "read worktree-done body: {}",
                acquire_retry::describe_reqwest_error(&e)
            ))
        }
    };
    match classify_retire_response(status, &text) {
        Ok(o) => Attempt::Done(Ok(o)),
        Err(t) => Attempt::Transient(t),
    }
}

/// Retire every row in turn under one `deadline`, logging each result with the
/// allocation's `agent_id`. Returns one result per row (`Err` = the budget ran
/// out on a transient failure). Awaited by the detached task, and directly by
/// tests.
pub(crate) async fn retire_rows(
    coord_http_base: &str,
    device_id: uuid::Uuid,
    agent_id: &str,
    rows: &[RetireRow],
    deadline: RetryDeadline,
) -> Vec<Result<RetireOutcome, String>> {
    let url = retire_url(coord_http_base, device_id);
    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        let body = retire_body(row);
        let r = acquire_retry::retry_transient(
            "worktree-done abandon",
            deadline,
            |_| retire_once(&url, &body),
            |e| e,
        )
        .await;
        match &r {
            Ok(RetireOutcome::Retired { shortfall }) => info!(
                agent_id = %agent_id,
                repo = %row.repo,
                worktree_path = %row.worktree_path,
                shortfall = ?shortfall,
                "abandoned allocation retired: row now reads abandoned on coord's ledger"
            ),
            Ok(other) => warn!(
                agent_id = %agent_id,
                repo = %row.repo,
                worktree_path = %row.worktree_path,
                outcome = ?other,
                "abandoned allocation NOT retired by coord; the row ages out through reclaim"
            ),
            Err(e) => warn!(
                agent_id = %agent_id,
                repo = %row.repo,
                worktree_path = %row.worktree_path,
                "abandoned allocation retire gave up (budget spent): {e}; the row ages out through reclaim"
            ),
        }
        results.push(r);
    }
    results
}

/// Hand back every row of an allocation the runner unwound after coord
/// answered, in a detached task. Never blocks, never changes `cause`.
pub(crate) fn spawn_retire_abandoned(
    coord_http_base: &str,
    device_id: uuid::Uuid,
    agent_id: &str,
    rows: Vec<RetireRow>,
    cause: &AllocateError,
) {
    if rows.is_empty() {
        return;
    }
    info!(
        agent_id = %agent_id,
        rows = rows.len(),
        "allocation unwound after coord answered ({cause}); retiring its rows"
    );
    let base = coord_http_base.to_string();
    let agent_id = agent_id.to_string();
    tokio::spawn(async move {
        let deadline = RetryDeadline::with_delays(
            RETIRE_BUDGET,
            tokio::time::Instant::now(),
            RETIRE_BASE_DELAY_SECS,
            RETIRE_CAP_DELAY_SECS,
        );
        retire_rows(&base, device_id, &agent_id, &rows, deadline).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    fn row() -> RetireRow {
        RetireRow {
            repo: "qontinui-runner".into(),
            worktree_path: "agent-worktrees/01a1/qontinui-runner".into(),
            branch: "agent/x/y".into(),
        }
    }

    #[test]
    fn request_path_and_body_send_coords_own_row() {
        let dev = uuid::Uuid::nil();
        assert_eq!(
            retire_url("https://coord.example/", dev),
            format!("https://coord.example/coord/worktree-done/{dev}")
        );
        assert_eq!(
            retire_body(&row()),
            serde_json::json!({
                "worktree_path": "agent-worktrees/01a1/qontinui-runner",
                "branch": "agent/x/y",
                "abandoned": true,
            })
        );
    }

    /// The shapes coord's `post_worktree_done` serves, field for field.
    #[test]
    fn response_vocabulary() {
        let ok = |b: &str| classify_retire_response(StatusCode::OK, b);
        assert_eq!(
            ok(
                r#"{"matched":1,"trigger_signal":"agent_done","statuses":["allocated"],"abandoned":1,"abandon_refused":null,"note":"retired"}"#
            ),
            Ok(RetireOutcome::Retired { shortfall: None })
        );
        assert_eq!(
            ok(
                r#"{"matched":2,"statuses":["allocated","allocated"],"abandoned":1,"abandon_refused":"retire_shortfall: 1 of 2 predicted row(s) moved"}"#
            ),
            Ok(RetireOutcome::Retired {
                shortfall: Some("retire_shortfall: 1 of 2 predicted row(s) moved".into())
            })
        );
        assert_eq!(
            ok(
                r#"{"matched":1,"statuses":["abandoned"],"abandoned":0,"abandon_refused":"no_retirable_row: statuses [abandoned]"}"#
            ),
            Ok(RetireOutcome::Refused(
                "no_retirable_row: statuses [abandoned]".into()
            ))
        );
        assert_eq!(
            ok(r#"{"matched":1,"statuses":["allocated"],"abandoned":0}"#),
            Ok(RetireOutcome::Refused(
                "coord moved no row and named no reason".into()
            ))
        );
        assert_eq!(
            ok(r#"{"matched":1,"abandon_refused":"r"}"#),
            Ok(RetireOutcome::Refused("r".into()))
        );
        assert_eq!(ok(r#"{"matched":1}"#), Ok(RetireOutcome::NotSupported));
        assert!(matches!(
            ok("<html>proxy</html>"),
            Ok(RetireOutcome::Unreadable(_))
        ));
        assert!(matches!(
            ok(r#"{"abandoned":"one"}"#),
            Ok(RetireOutcome::Unreadable(_))
        ));
        assert_eq!(
            classify_retire_response(StatusCode::NOT_FOUND, r#"{"matched":0,"abandoned":0}"#),
            Ok(RetireOutcome::NoRow)
        );
        assert!(matches!(
            classify_retire_response(StatusCode::BAD_REQUEST, "bad"),
            Ok(RetireOutcome::Rejected(_))
        ));
        assert!(classify_retire_response(StatusCode::SERVICE_UNAVAILABLE, "down").is_err());
    }

    /// The field this module used to read. Coord has never sent it, so a body
    /// carrying only it is a coord with no retire count, not a retire.
    #[test]
    fn a_status_field_alone_is_not_a_retire() {
        assert_eq!(
            classify_retire_response(StatusCode::OK, r#"{"matched":1,"status":"abandoned"}"#),
            Ok(RetireOutcome::NotSupported)
        );
    }

    /// Coord's error arm and its fail-closed refusals moved nothing because
    /// coord was failing: retried, like a 5xx.
    #[test]
    fn coord_failing_to_apply_or_decide_is_transient() {
        let cause = classify_retire_response(
            StatusCode::OK,
            r#"{"matched":0,"statuses":[],"abandoned":0,"persisted":false,"note":"stamp deferred (transient PG error or pre-migration column)"}"#,
        )
        .unwrap_err();
        assert!(
            cause.contains("persisted:false") && cause.contains("stamp deferred"),
            "{cause}"
        );
        for reason in [
            "retire_failed_closed: the device census-POST presence probe errored (Redis); nothing was retired",
            "retire_failed_closed: the census-gated UPDATE errored (PG pool acquire: timed out); nothing was retired",
        ] {
            let body = serde_json::json!({"matched": 1, "abandoned": 0, "abandon_refused": reason});
            let cause = classify_retire_response(StatusCode::OK, &body.to_string()).unwrap_err();
            assert!(cause.contains("retire_failed_closed"), "{cause}");
        }
        // The same prefix on a misconfigured window is a configuration: final.
        let body = serde_json::json!({"matched": 1, "abandoned": 0, "abandon_refused":
            "retire_failed_closed: census presence window is <= 0 (0s); the gate cannot be evaluated"});
        assert!(matches!(
            classify_retire_response(StatusCode::OK, &body.to_string()),
            Ok(RetireOutcome::Refused(_))
        ));
    }

    #[test]
    fn captured_rows_are_coords_rows_verbatim() {
        let w: CoordAllocatedWorktree = serde_json::from_value(serde_json::json!({
            "repo": "qontinui-schemas",
            "branch": "agent/b",
            "parent_sha": "abc",
            "worktree_path": "agent-worktrees/a/qontinui-schemas",
            "status": "allocated",
            "origin": "declared_sibling",
        }))
        .unwrap();
        assert_eq!(
            rows_to_retire(&[w]),
            vec![RetireRow {
                repo: "qontinui-schemas".into(),
                worktree_path: "agent-worktrees/a/qontinui-schemas".into(),
                branch: "agent/b".into(),
            }]
        );
    }

    #[tokio::test]
    async fn the_loop_stops_on_a_typed_refusal() {
        let _amb = super::super::test_coord::isolate();
        let coord = super::super::test_coord::ScriptedCoord::start(vec![(
            "/coord/worktree-done/",
            vec![
                (503, "{}".to_string()),
                (
                    200,
                    r#"{"matched":1,"abandoned":0,"persisted":false,"note":"stamp deferred"}"#
                        .to_string(),
                ),
                (
                    200,
                    r#"{"matched":1,"abandoned":0,"abandon_refused":"x"}"#.to_string(),
                ),
            ],
        )])
        .await;
        // A zero-second delay sequence: the loop's shape, not its patience.
        let deadline =
            RetryDeadline::with_delays(Duration::from_secs(30), tokio::time::Instant::now(), 0, 0);
        let out = retire_rows(
            &coord.base,
            uuid::Uuid::nil(),
            "agent-1",
            &[row()],
            deadline,
        )
        .await;
        assert!(
            matches!(out.as_slice(), [Ok(RetireOutcome::Refused(_))]),
            "{out:?}"
        );
        assert_eq!(coord.hits("/coord/worktree-done/"), 3);
    }

    #[tokio::test]
    async fn the_loop_stops_on_its_budget() {
        let _amb = super::super::test_coord::isolate();
        let coord = super::super::test_coord::ScriptedCoord::start(vec![(
            "/coord/worktree-done/",
            vec![(503, "{}".to_string())],
        )])
        .await;
        let start = tokio::time::Instant::now();
        let budget = Duration::from_secs(3);
        let deadline = RetryDeadline::with_delays(budget, start, 1, 1);
        let out = retire_rows(
            &coord.base,
            uuid::Uuid::nil(),
            "agent-1",
            &[row()],
            deadline,
        )
        .await;
        assert!(matches!(out.as_slice(), [Err(_)]), "{out:?}");
        let hits = coord.hits("/coord/worktree-done/");
        assert!(hits >= 2, "retried at least once: {hits}");
        // The last attempt starts before the deadline; it may take one
        // attempt timeout to answer, never another full delay.
        assert!(tokio::time::Instant::now() <= start + budget + RETIRE_ATTEMPT_TIMEOUT);
    }
}
