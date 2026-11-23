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
//! starting at 30 s, capped at 5 min, inside a 30 min budget. A final answer
//! (a retired count, a refusal reason, no row, a coord that does not implement
//! the flag) ends the loop. Two 200 answers do not, because each says coord
//! applied nothing it could judge: `persisted: false` (the stamp itself failed)
//! and a refusal whose reason code says coord could not evaluate its own gate
//! ([`REFUSALS_COORD_COULD_NOT_EVALUATE`]). A runner exit drops the task and
//! the row ages out as before.
//!
//! The answer is read from the fields coord's `post_worktree_done` serves:
//! `abandoned` (the retire UPDATE's own row count), `abandon_refused` (why it
//! was not more) and, on its error arm, `persisted: false`. It serves no
//! `status` field. Until 2026-11-08 this module read one, so every successful
//! retire was logged as "NOT retired" and a transient stamp failure ended the
//! loop on its first attempt.
//!
//! No directory is removed here: `abandoned` grants no disk authority, and this
//! unwind only releases claims and retires rows. The one runner path that
//! removes a just-allocated tree is a terminal door's spawn refusal
//! ([`super::remove_unspawned`], via `IsolatedEditContext::remove_unspawned`),
//! which holds the fact `abandoned` cannot carry — that no child ever ran in
//! it. Every other tree stays with reclaim.
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

/// The request body: coord's own `worktree_path` for this one row, plus
/// `abandoned: true`. The branch is NOT sent. Coord matches
/// `worktree_path = $path OR branch = $branch`, a union, and a declared
/// sibling shares its allocation's branch, so sending both would retire every
/// row of the allocation on the first POST, and each later row's POST would
/// then read `no_retirable_row` and be reported as not retired. One field
/// names one row (coord's `WorktreeDoneRequest` doc).
pub(crate) fn retire_body(row: &RetireRow) -> serde_json::Value {
    serde_json::json!({
        "worktree_path": row.worktree_path,
        "abandoned": true,
    })
}

/// What coord said about one retire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetireOutcome {
    /// HTTP 200 with `abandoned` of 1 or more: the retire UPDATE moved that
    /// many rows. `note` is coord's `abandon_refused` when it sent one beside
    /// the count, which makes this a partial retire or one that moved more
    /// rows than coord predicted.
    Retired {
        abandoned: u64,
        note: Option<String>,
    },
    /// HTTP 200 with `abandoned: 0` and `no_retirable_row`, where every
    /// matched row already reads `abandoned`: an earlier attempt retired it and
    /// its answer was lost, or another caller did.
    AlreadyRetired,
    /// HTTP 200 with `abandoned: 0` and a final reason: coord evaluated its
    /// gate and retired nothing.
    Refused(String),
    /// HTTP 200 with no `abandoned` count at all: a coord that predates the
    /// flag.
    NotSupported,
    /// HTTP 404: coord holds no row for this path on this device. Its terminal
    /// sweep deleted it, or it was never written. An abandoned row that still
    /// exists answers 200 `no_retirable_row` instead.
    NoRow,
    /// Any other 4xx, or a 200 whose body could not be read as coord's
    /// answer: a final answer.
    Rejected(String),
}

/// The `abandon_refused` reason codes (the token before the first `:`) that
/// mean coord could NOT evaluate its retire gate, so nothing was judged and
/// another attempt CAN succeed, though not every cause clears inside the
/// retire budget. `retire_failed_closed` is an errored census UPDATE or
/// presence probe, which a later attempt can pass, or a misconfigured presence
/// window, which it cannot. `census_silent_for_device` is coord holding no
/// recent census post from this device: this runner's next post (every 300 s)
/// ends it when the posts were merely late, but not when coord's persist of a
/// post failed (that marker is sticky for the whole presence window, 7200 s by
/// default) or when this device has no worktree left and its census post
/// cannot arm presence. Either way the cost is bounded retries, and the loop
/// then logs that it gave up. Every other code (`no_retirable_row`,
/// `retire_shortfall`, `retire_matched_more_than_predicted`) is a judgement.
/// These are coord's typed codes, defined in its `agent_worktrees.rs`; they
/// are not error text.
const REFUSALS_COORD_COULD_NOT_EVALUATE: &[&str] =
    &["retire_failed_closed", "census_silent_for_device"];

/// The reason code of an `abandon_refused` string: the token before its first
/// `:`, or the whole string when it has none.
fn refusal_code(reason: &str) -> &str {
    reason
        .split_once(':')
        .map_or(reason, |(code, _)| code)
        .trim()
}

/// Whether every row coord matched already read `abandoned` before this call
/// (`statuses` is the matched rows' status before the retire ran).
fn all_already_abandoned(v: &serde_json::Value) -> bool {
    match v.get("statuses").and_then(serde_json::Value::as_array) {
        Some(s) if !s.is_empty() => s.iter().all(|x| x.as_str() == Some("abandoned")),
        _ => false,
    }
}

/// Classify one fully read retire response. `Err` is a transient answer worth
/// another attempt: a 5xx, a stamp that persisted nothing, or a refusal coord
/// could not evaluate. Pure, so the vocabulary is testable.
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
        _ => {
            return Ok(RetireOutcome::Rejected(format!(
                "HTTP {} whose body is not coord's JSON answer: {}",
                status.as_u16(),
                one_line(body_text)
            )))
        }
    };
    // The stamp's own error arm: coord accepted the call and wrote nothing, so
    // the row is unchanged and the same request can be sent again.
    if v.get("persisted").and_then(serde_json::Value::as_bool) == Some(false) {
        let note = v
            .get("note")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        return Err(format!(
            "worktree-done applied nothing (persisted: false): {}",
            one_line(note)
        ));
    }
    // `null` and absent both mean "no reason".
    let refused = v
        .get("abandon_refused")
        .filter(|r| !r.is_null())
        .map(|r| match r.as_str() {
            Some(s) => one_line(s),
            None => one_line(&r.to_string()),
        });
    let abandoned = match v.get("abandoned") {
        None => return Ok(RetireOutcome::NotSupported),
        Some(a) => match a.as_u64() {
            Some(n) => n,
            None => {
                return Ok(RetireOutcome::Rejected(format!(
                    "HTTP {} whose `abandoned` is not a count: {}",
                    status.as_u16(),
                    one_line(&a.to_string())
                )))
            }
        },
    };
    if abandoned > 0 {
        return Ok(RetireOutcome::Retired {
            abandoned,
            note: refused,
        });
    }
    match refused {
        Some(r) if REFUSALS_COORD_COULD_NOT_EVALUATE.contains(&refusal_code(&r)) => {
            Err(format!("worktree-done could not evaluate the retire: {r}"))
        }
        Some(r) if refusal_code(&r) == "no_retirable_row" && all_already_abandoned(&v) => {
            Ok(RetireOutcome::AlreadyRetired)
        }
        Some(r) => Ok(RetireOutcome::Refused(r)),
        None => Ok(RetireOutcome::Refused(
            "abandoned: 0 and coord named no reason".to_string(),
        )),
    }
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
            Ok(RetireOutcome::Retired {
                abandoned,
                note: None,
            }) => info!(
                agent_id = %agent_id,
                repo = %row.repo,
                worktree_path = %row.worktree_path,
                abandoned = abandoned,
                "abandoned allocation retired: row now reads abandoned on coord's ledger"
            ),
            Ok(RetireOutcome::Retired {
                abandoned,
                note: Some(note),
            }) => {
                let what = match refusal_code(note) {
                    "retire_shortfall" => "PARTLY retired",
                    "retire_matched_more_than_predicted" => {
                        "retired, and coord moved MORE rows than it predicted"
                    }
                    _ => "retired, with a note from coord",
                };
                warn!(
                    agent_id = %agent_id,
                    repo = %row.repo,
                    worktree_path = %row.worktree_path,
                    abandoned = abandoned,
                    "abandoned allocation {what}: {note}"
                )
            }
            Ok(RetireOutcome::AlreadyRetired) => info!(
                agent_id = %agent_id,
                repo = %row.repo,
                worktree_path = %row.worktree_path,
                "abandoned allocation already reads abandoned on coord's ledger"
            ),
            Ok(other) => warn!(
                agent_id = %agent_id,
                repo = %row.repo,
                worktree_path = %row.worktree_path,
                branch = %row.branch,
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
                "abandoned": true,
            })
        );
    }

    /// coord's `post_worktree_done` 200 body, built the way its
    /// `worktree_reclaim.rs` builds it: `matched` is the number of rows the
    /// path matched, `statuses` their status before the call, and `note`
    /// follows `abandoned`.
    fn coord_answer_for(
        statuses: &[&str],
        abandoned: u64,
        abandon_refused: Option<&str>,
    ) -> String {
        let note = if abandoned > 0 {
            "retired: status='abandoned' — a LEDGER fact only; the existing terminal sweep deletes the row later, still behind its census and pin guards"
        } else {
            "accelerated candidacy; coord decides safety, the runner executes"
        };
        serde_json::json!({
            "matched": statuses.len(),
            "trigger_signal": "agent_done",
            "statuses": statuses,
            "abandoned": abandoned,
            "abandon_refused": abandon_refused,
            "note": note,
        })
        .to_string()
    }

    /// The common case: one matched row that read `allocated`.
    fn coord_answer(abandoned: u64, abandon_refused: Option<&str>) -> String {
        coord_answer_for(&["allocated"], abandoned, abandon_refused)
    }

    /// coord's error arm: the stamp failed and nothing was written.
    const COORD_STAMP_DEFERRED: &str = r#"{"matched":0,"statuses":[],"abandoned":0,"persisted":false,"note":"stamp deferred (transient PG error or pre-migration column)"}"#;

    /// An outcome as its `Debug` text. A retire is compared this way, not by
    /// constructing `Retired { .. }`, so these tests still build when the
    /// arming proof reverts the production code to a base whose `Retired`
    /// carried no fields, and the proof observes the assertion failing rather
    /// than the build.
    fn shown<T: std::fmt::Debug>(outcome: &T) -> String {
        format!("{outcome:?}")
    }

    #[test]
    fn response_vocabulary() {
        assert_eq!(
            shown(&classify_retire_response(
                StatusCode::OK,
                &coord_answer(1, None)
            )),
            "Ok(Retired { abandoned: 1, note: None })"
        );
        // A count beside a reason is a partial retire, not a refusal.
        assert_eq!(
            shown(&classify_retire_response(
                StatusCode::OK,
                &coord_answer_for(
                    &["allocated", "allocated"],
                    1,
                    Some("retire_shortfall: 1 of 2 predicted row(s) moved")
                )
            )),
            "Ok(Retired { abandoned: 1, note: Some(\"retire_shortfall: 1 of 2 predicted row(s) moved\") })"
        );
        // A row an earlier attempt (or another caller) already retired.
        assert_eq!(
            shown(&classify_retire_response(
                StatusCode::OK,
                &coord_answer_for(
                    &["abandoned"],
                    0,
                    Some("no_retirable_row: statuses [\"abandoned\"]")
                )
            )),
            "Ok(AlreadyRetired)"
        );
        // Coord answers no_retirable_row only when no matched row is
        // allocated or active, so each reason rides the statuses it comes with.
        // The mixed case is not a retire already done: one row is merged.
        for (statuses, final_reason) in [
            (&["merged"][..], "no_retirable_row: statuses [\"merged\"]"),
            (
                &["abandoned", "merged"][..],
                "no_retirable_row: statuses [\"abandoned\", \"merged\"]",
            ),
            (
                &["allocated"][..],
                "retire_shortfall: 0 of 1 predicted row(s) moved",
            ),
        ] {
            assert_eq!(
                classify_retire_response(
                    StatusCode::OK,
                    &coord_answer_for(statuses, 0, Some(final_reason))
                ),
                Ok(RetireOutcome::Refused(final_reason.into())),
                "{final_reason}"
            );
        }
        // Coord could not evaluate its gate: worth another attempt.
        for unevaluated in [
            "retire_failed_closed: the census-gated UPDATE errored (pg down); nothing was retired",
            "census_silent_for_device: coord cannot establish that this device's census was published",
        ] {
            assert!(
                classify_retire_response(StatusCode::OK, &coord_answer(0, Some(unevaluated)))
                    .is_err(),
                "{unevaluated}"
            );
        }
        assert_eq!(
            classify_retire_response(StatusCode::OK, &coord_answer(0, None)),
            Ok(RetireOutcome::Refused(
                "abandoned: 0 and coord named no reason".into()
            ))
        );
        // The stamp persisted nothing: retry, never "not supported".
        assert!(classify_retire_response(StatusCode::OK, COORD_STAMP_DEFERRED).is_err());
        // No `abandoned` count: a coord that predates the flag.
        assert_eq!(
            classify_retire_response(
                StatusCode::OK,
                r#"{"matched":1,"trigger_signal":"agent_done","statuses":["allocated"],"note":"x"}"#
            ),
            Ok(RetireOutcome::NotSupported)
        );
        // coord serves no `status` field, so one is never read as an answer.
        assert_eq!(
            classify_retire_response(StatusCode::OK, r#"{"matched":1,"status":"abandoned"}"#),
            Ok(RetireOutcome::NotSupported)
        );
        assert!(matches!(
            classify_retire_response(StatusCode::OK, "<html>a proxy's page</html>"),
            Ok(RetireOutcome::Rejected(_))
        ));
        assert!(matches!(
            classify_retire_response(StatusCode::OK, r#"{"abandoned":"1"}"#),
            Ok(RetireOutcome::Rejected(_))
        ));
        assert_eq!(
            classify_retire_response(StatusCode::NOT_FOUND, "no_retirable_row"),
            Ok(RetireOutcome::NoRow)
        );
        assert!(matches!(
            classify_retire_response(StatusCode::BAD_REQUEST, "bad"),
            Ok(RetireOutcome::Rejected(_))
        ));
        assert!(classify_retire_response(StatusCode::SERVICE_UNAVAILABLE, "down").is_err());
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
                    coord_answer_for(
                        &["merged"],
                        0,
                        Some("no_retirable_row: statuses [\"merged\"]"),
                    ),
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
        assert_eq!(coord.hits("/coord/worktree-done/"), 2);
    }

    /// The two 200 answers that say coord applied nothing it could judge are
    /// retried, and the retry then sees the real retire.
    #[tokio::test]
    async fn the_loop_retries_a_retire_coord_applied_nothing_to() {
        for first in [
            COORD_STAMP_DEFERRED.to_string(),
            coord_answer(
                0,
                Some("retire_failed_closed: the device census-POST presence probe errored (redis); nothing was retired"),
            ),
            coord_answer(
                0,
                Some("census_silent_for_device: coord cannot establish that this device's census was published"),
            ),
        ] {
            let _amb = super::super::test_coord::isolate();
            let coord = super::super::test_coord::ScriptedCoord::start(vec![(
                "/coord/worktree-done/",
                vec![(200, first.clone()), (200, coord_answer(1, None))],
            )])
            .await;
            let deadline = RetryDeadline::with_delays(
                Duration::from_secs(30),
                tokio::time::Instant::now(),
                0,
                0,
            );
            let out = retire_rows(
                &coord.base,
                uuid::Uuid::nil(),
                "agent-1",
                &[row()],
                deadline,
            )
            .await;
            assert_eq!(
                coord.hits("/coord/worktree-done/"),
                2,
                "first answer: {first}"
            );
            assert_eq!(
                shown(&out),
                "[Ok(Retired { abandoned: 1, note: None })]",
                "first answer: {first}"
            );
        }
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
