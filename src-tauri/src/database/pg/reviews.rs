//! PostgreSQL CRUD operations for the `reviews` table (migration v31).
//!
//! See §2 of `qontinui-dev-notes/plans/productivity-stack.md` for the data
//! model. Each row is a verdict from an `/auto-review` reviewer agent over a
//! worker session's task. A task may have multiple review rows (re-review
//! after a `needs_fix` cycle); the latest by `created_at` is operative.
//!
//! Confidence is on `[0, 1]`. The Coordinator's Rule D consumes these rows
//! and decides whether the work auto-merges (`approved` AND
//! `confidence >= 0.85`), queues for user approval (`approved` AND
//! `0.7 <= confidence < 0.85`), is re-assigned (`needs_fix`, bounded retries
//! via [`PgDb::count_needs_fix_for_task`]), or escalates.
//!
//! UUID columns are cast to/from TEXT in queries because the runner does
//! not enable tokio-postgres `with-uuid-1`. Same convention as
//! `pg::plans` / `pg::tasks` / `pg::coordinator_decisions`.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// One row from `reviews`. The TS contract in productivity-stack §8 lists
/// this as `ReviewRow` with `camelCase` fields; the `serde(rename_all)`
/// attribute lets Tauri commands return this type directly to the React
/// frontend without an additional mapping layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRow {
    pub id: String,
    pub task_id: String,
    pub reviewer_session_id: String,
    pub reviewed_session_id: String,
    /// One of `approved` | `needs_fix` | `escalate`.
    pub verdict: String,
    pub confidence: f64,
    pub reasoning: String,
    pub diff_summary: Option<serde_json::Value>,
    pub test_results: Option<serde_json::Value>,
    /// `approved` when the user accepts the recommendation; `rejected` when
    /// they decline; `null` until the user has acted (or for high-confidence
    /// auto-merges that bypass the recommendations queue entirely).
    pub user_decision: Option<String>,
    pub user_decided_at: Option<String>,
    pub created_at: String,
}

/// Input shape for [`PgDb::insert_review`]. The DB allocates the UUID and
/// stamps `created_at`. `user_decision`/`user_decided_at` are not settable
/// on insert — they're updated later via
/// [`PgDb::record_review_user_decision`].
#[derive(Debug, Clone)]
pub struct InsertReviewInput<'a> {
    pub task_id: &'a str,
    pub reviewer_session_id: &'a str,
    pub reviewed_session_id: &'a str,
    pub verdict: &'a str,
    pub confidence: f64,
    pub reasoning: &'a str,
    pub diff_summary: Option<serde_json::Value>,
    pub test_results: Option<serde_json::Value>,
}

/// Typed errors from [`PgDb::insert_review`] so the HTTP layer can map them
/// to specific status codes (409 for self-review, 400 for malformed input,
/// 500 otherwise).
#[derive(Debug, Clone)]
pub enum InsertReviewError {
    /// `reviewer_session_id == reviewed_session_id`. Per plan §4 Rules:
    /// "Don't review yourself." Maps to HTTP 409 at the endpoint.
    SelfReview,
    /// `verdict` was not one of the recognised values.
    InvalidVerdict(String),
    /// `confidence` was outside `[0.0, 1.0]`.
    InvalidConfidence(f64),
    /// Underlying DB error.
    Db(String),
}

impl std::fmt::Display for InsertReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InsertReviewError::SelfReview => write!(
                f,
                "reviewer_session_id == reviewed_session_id (no self-review)"
            ),
            InsertReviewError::InvalidVerdict(v) => write!(
                f,
                "invalid verdict '{}' (expected approved|needs_fix|escalate)",
                v
            ),
            InsertReviewError::InvalidConfidence(c) => {
                write!(f, "invalid confidence {} (must be in [0.0, 1.0])", c)
            }
            InsertReviewError::Db(e) => write!(f, "{}", e),
        }
    }
}

const SELECT_COLS: &str = r#"
    id::text, task_id::text, reviewer_session_id, reviewed_session_id,
    verdict, confidence, reasoning, diff_summary, test_results,
    user_decision, user_decided_at::text, created_at::text
"#;

fn row_to_review(r: &tokio_postgres::Row) -> ReviewRow {
    ReviewRow {
        id: r.get(0),
        task_id: r.get(1),
        reviewer_session_id: r.get(2),
        reviewed_session_id: r.get(3),
        verdict: r.get(4),
        confidence: r.get(5),
        reasoning: r.get(6),
        diff_summary: r.get(7),
        test_results: r.get(8),
        user_decision: r.get(9),
        user_decided_at: r.get(10),
        created_at: r.get(11),
    }
}

fn is_valid_verdict(v: &str) -> bool {
    matches!(v, "approved" | "needs_fix" | "escalate")
}

impl PgDb {
    /// Insert a single review row. Rejects self-review with
    /// [`InsertReviewError::SelfReview`] (HTTP 409 at the caller).
    pub async fn insert_review(
        &self,
        input: &InsertReviewInput<'_>,
    ) -> Result<ReviewRow, InsertReviewError> {
        if input.reviewer_session_id == input.reviewed_session_id {
            return Err(InsertReviewError::SelfReview);
        }
        if !is_valid_verdict(input.verdict) {
            return Err(InsertReviewError::InvalidVerdict(input.verdict.to_string()));
        }
        if !input.confidence.is_finite()
            || input.confidence < 0.0
            || input.confidence > 1.0
        {
            return Err(InsertReviewError::InvalidConfidence(input.confidence));
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| InsertReviewError::Db(format!("PG pool error: {}", e)))?;

        let row = conn
            .query_one(
                &format!(
                    r#"
                    INSERT INTO reviews (
                        task_id, reviewer_session_id, reviewed_session_id,
                        verdict, confidence, reasoning,
                        diff_summary, test_results
                    )
                    VALUES (
                        $1::uuid, $2, $3, $4, $5, $6, $7, $8
                    )
                    RETURNING {}
                    "#,
                    SELECT_COLS
                ),
                &[
                    &input.task_id,
                    &input.reviewer_session_id,
                    &input.reviewed_session_id,
                    &input.verdict,
                    &input.confidence,
                    &input.reasoning,
                    &input.diff_summary,
                    &input.test_results,
                ],
            )
            .await
            .map_err(|e| InsertReviewError::Db(format!("Failed to insert review: {}", e)))?;

        Ok(row_to_review(&row))
    }

    /// Look up a single review by id.
    pub async fn get_review_by_id(&self, review_id: &str) -> Result<Option<ReviewRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                &format!(
                    "SELECT {} FROM reviews WHERE id = $1::uuid",
                    SELECT_COLS
                ),
                &[&review_id],
            )
            .await
            .map_err(|e| format!("Failed to get review: {}", e))?;

        Ok(row.as_ref().map(row_to_review))
    }

    /// Latest review for a worker session (by `reviewed_session_id`). Used
    /// by `ReviewBadge`'s 30-second poll fallback and the `/sessions/<id>/
    /// latest-review` HTTP endpoint.
    pub async fn get_latest_review_for_session(
        &self,
        reviewed_session_id: &str,
    ) -> Result<Option<ReviewRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                &format!(
                    r#"
                    SELECT {}
                    FROM reviews
                    WHERE reviewed_session_id = $1
                    ORDER BY created_at DESC
                    LIMIT 1
                    "#,
                    SELECT_COLS
                ),
                &[&reviewed_session_id],
            )
            .await
            .map_err(|e| format!("Failed to get latest review for session: {}", e))?;

        Ok(row.as_ref().map(row_to_review))
    }

    /// All reviews for a task, ordered newest-first. The first row is the
    /// operative verdict; older rows are the historical re-review trail.
    pub async fn get_reviews_for_task(&self, task_id: &str) -> Result<Vec<ReviewRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM reviews
                    WHERE task_id = $1::uuid
                    ORDER BY created_at DESC
                    "#,
                    SELECT_COLS
                ),
                &[&task_id],
            )
            .await
            .map_err(|e| format!("Failed to list reviews for task: {}", e))?;

        Ok(rows.iter().map(row_to_review).collect())
    }

    /// Reviews created within the last `within_seconds` seconds, newest-first,
    /// capped at `limit`. Used by `/coordinate` Rule D to scan the latest
    /// iteration's reviews and decide whether to merge / re-assign / escalate.
    pub async fn list_recent_reviews(
        &self,
        within_seconds: i64,
        limit: i64,
    ) -> Result<Vec<ReviewRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM reviews
                    WHERE created_at >= NOW() - make_interval(secs => $1::double precision)
                    ORDER BY created_at DESC
                    LIMIT $2
                    "#,
                    SELECT_COLS
                ),
                &[&(within_seconds as f64), &limit],
            )
            .await
            .map_err(|e| format!("Failed to list recent reviews: {}", e))?;

        Ok(rows.iter().map(row_to_review).collect())
    }

    /// Count how many `needs_fix` rows exist for a task. Used by Rule D to
    /// enforce the 3-retry cap before escalating.
    pub async fn count_needs_fix_for_task(&self, task_id: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"
                SELECT COUNT(*)::bigint
                FROM reviews
                WHERE task_id = $1::uuid AND verdict = 'needs_fix'
                "#,
                &[&task_id],
            )
            .await
            .map_err(|e| format!("Failed to count needs_fix reviews: {}", e))?;

        Ok(row.get(0))
    }

    /// Reviews in the auto-merge gate band (`approved`, `0.7 <= confidence
    /// < 0.85`) that the user has not yet decided on. Used by the
    /// Recommendations queue on the Coordinator dashboard.
    pub async fn list_pending_recommendations(&self) -> Result<Vec<ReviewRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM reviews
                    WHERE verdict = 'approved'
                      AND confidence >= 0.7
                      AND confidence < 0.85
                      AND user_decision IS NULL
                    ORDER BY created_at DESC
                    "#,
                    SELECT_COLS
                ),
                &[],
            )
            .await
            .map_err(|e| format!("Failed to list pending recommendations: {}", e))?;

        Ok(rows.iter().map(row_to_review).collect())
    }

    /// Record a user's approve/reject decision on a recommendation. Returns
    /// `true` if the row updated (existed and was not already decided).
    pub async fn record_review_user_decision(
        &self,
        review_id: &str,
        decision: &str,
    ) -> Result<bool, String> {
        if !matches!(decision, "approved" | "rejected") {
            return Err(format!(
                "invalid user decision '{}' (expected approved|rejected)",
                decision
            ));
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE reviews
                SET user_decision = $2,
                    user_decided_at = NOW()
                WHERE id = $1::uuid
                  AND user_decision IS NULL
                "#,
                &[&review_id, &decision],
            )
            .await
            .map_err(|e| format!("Failed to record review decision: {}", e))?;

        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_validator_accepts_canonical_strings() {
        assert!(is_valid_verdict("approved"));
        assert!(is_valid_verdict("needs_fix"));
        assert!(is_valid_verdict("escalate"));
        assert!(!is_valid_verdict("APPROVED"));
        assert!(!is_valid_verdict("ok"));
        assert!(!is_valid_verdict(""));
    }

    #[test]
    fn insert_review_error_display_contains_actionable_message() {
        let s = format!("{}", InsertReviewError::SelfReview);
        assert!(s.contains("self-review"));

        let s = format!("{}", InsertReviewError::InvalidVerdict("OK".to_string()));
        assert!(s.contains("invalid verdict"));

        let s = format!("{}", InsertReviewError::InvalidConfidence(1.5));
        assert!(s.contains("invalid confidence"));
    }
}
