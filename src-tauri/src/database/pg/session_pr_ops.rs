//! Database operations for the runner-local per-session PR projection
//! (`project.session_prs`).
//!
//! Hand-written tokio-postgres (NOT Clorinde), mirroring the connection idiom
//! in [`super::pr_watch_ops`] (`let conn = self.pool.get().await …;
//! conn.query(...)`). The table is a single-consumer, runner-local projection:
//! the attribution reconciler ([`crate::session_pr_reconciler`]) writes it and
//! the Terminal zone-header dropdown command
//! ([`crate::commands::session_prs`]) reads it — no cross-component reader, so
//! there is no companion web migration (see the self-heal block in
//! [`super`]`::verify_and_provision`).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::PgDb;

/// One row of the per-session PR projection. Columns map 1:1 to
/// `project.session_prs`.
#[derive(Debug, Clone)]
pub struct SessionPrRow {
    pub claude_session_id: Uuid,
    /// `"owner/name"`.
    pub repo: String,
    pub pr_number: i64,
    pub head_branch: Option<String>,
    /// GitHub-ish lifecycle: `"open"` | `"closed"` | `"merged"`.
    pub pr_state: Option<String>,
    /// The LAND verdict, not GitHub's `merged` boolean — see
    /// [`SessionPrStatus::landed`].
    pub merged: bool,
    /// GitHub's own merge timestamp. `None` for a fast-forward land.
    pub merged_at: Option<DateTime<Utc>>,
    /// Which land signal decided the verdict — see
    /// [`SessionPrStatus::land_signal`].
    pub land_signal: Option<String>,
    /// Why a land verdict could not be established; set iff
    /// `land_signal == "land-unknown"`.
    pub land_reason: Option<String>,
    /// When the PR landed, by whichever signal proved it.
    pub landed_at: Option<DateTime<Utc>>,
    /// When the runner-local projection FIRST attributed this PR to the session
    /// (`created_at`). This is an OBSERVATION time, not GitHub's PR creation
    /// time — the runner never mirrors that column, and naming it `opened_at`
    /// on the wire without saying so would invent a precision the row does not
    /// have. `Option` although the column is `NOT NULL`: a read is not the
    /// place to panic on a schema surprise.
    pub created_at: Option<DateTime<Utc>>,
}

/// The lifecycle + land columns both write paths carry.
///
/// Grouped into one value deliberately: the reconciler has two write paths
/// (branch attribution and the no-live-branch status refresh) and a positional
/// argument list let a new column be silently dropped on one of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPrStatus {
    /// `"open"` | `"closed"` | `"merged"` — the label the dropdown renders.
    pub pr_state: Option<String>,
    /// True iff SOME land signal PROVED the PR landed — GitHub's `merged`
    /// boolean, a local content proof (`ff-land`), or coord. Written to the
    /// `merged` column, whose meaning is therefore "landed", widened when the
    /// land-signal cascade shipped: coord fast-forward lands are the majority
    /// of this fleet's landings and leave GitHub's `merged` false.
    pub landed: bool,
    /// GitHub's `merged_at`. `None` for a fast-forward land — the honest land
    /// stamp for those is [`Self::landed_at`].
    pub merged_at: Option<DateTime<Utc>>,
    /// `"github-merge"` | `"ff-land"` | `"coord-label"` | `"not-landed"` |
    /// `"land-unknown"`.
    pub land_signal: Option<String>,
    /// REQUIRED when `land_signal == "land-unknown"`: `"rebase_land_or_abandoned"`
    /// (the ancestry test failed, which a coord rebase-land and an abandoned
    /// PR produce alike) | `"ref_stale"` |
    /// `"head_object_missing"` | `"no_base_ref"` | `"not_a_repo"`. A land
    /// verdict that could not be evaluated is never reported as a confident
    /// negative.
    pub land_reason: Option<String>,
    /// When the PR landed, by whichever signal proved it.
    pub landed_at: Option<DateTime<Utc>>,
}

impl PgDb {
    /// Insert or refresh one attributed PR for a session. Idempotent on the
    /// `(claude_session_id, repo, pr_number)` primary key — a re-observed PR
    /// updates its branch/state/land fields and bumps `updated_at`.
    ///
    /// A LAND IS MONOTONE. Once a signal proved the PR landed, a later tick
    /// that can no longer re-prove it must not demote the row: the content
    /// proof reads local git objects, and a post-land branch delete followed by
    /// a `git gc` genuinely removes the head commit, which would otherwise flip
    /// a proven `ff-land` back to `land-unknown`. So every land column keeps
    /// its stored value whenever the row is already landed and the incoming
    /// verdict is not.
    pub async fn upsert_session_pr(
        &self,
        claude_session_id: Uuid,
        repo: &str,
        pr_number: i64,
        head_branch: Option<&str>,
        status: &SessionPrStatus,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "INSERT INTO project.session_prs AS sp \
                 (claude_session_id, repo, pr_number, head_branch, pr_state, merged, merged_at, \
                  land_signal, land_reason, landed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (claude_session_id, repo, pr_number) DO UPDATE SET \
                 head_branch = EXCLUDED.head_branch, \
                 pr_state    = CASE WHEN sp.merged AND NOT EXCLUDED.merged \
                                    THEN sp.pr_state    ELSE EXCLUDED.pr_state    END, \
                 merged      = sp.merged OR EXCLUDED.merged, \
                 merged_at   = COALESCE(EXCLUDED.merged_at, sp.merged_at), \
                 land_signal = CASE WHEN sp.merged AND NOT EXCLUDED.merged \
                                    THEN sp.land_signal ELSE EXCLUDED.land_signal END, \
                 land_reason = CASE WHEN sp.merged AND NOT EXCLUDED.merged \
                                    THEN sp.land_reason ELSE EXCLUDED.land_reason END, \
                 landed_at   = CASE WHEN sp.merged AND NOT EXCLUDED.merged \
                                    THEN sp.landed_at \
                                    ELSE COALESCE(EXCLUDED.landed_at, sp.landed_at) END, \
                 updated_at  = now()",
            &[
                &claude_session_id,
                &repo,
                &pr_number,
                &head_branch,
                &status.pr_state,
                &status.landed,
                &status.merged_at,
                &status.land_signal,
                &status.land_reason,
                &status.landed_at,
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_session_pr: {}", e))?;

        Ok(())
    }

    /// Every PR attributed to `claude_session_id`, unmerged-first then newest
    /// PR number first — the order the Terminal dropdown renders verbatim.
    pub async fn list_session_prs(
        &self,
        claude_session_id: Uuid,
    ) -> Result<Vec<SessionPrRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT claude_session_id, repo, pr_number, head_branch, pr_state, merged, \
                        merged_at, land_signal, land_reason, landed_at, created_at \
                 FROM project.session_prs \
                 WHERE claude_session_id = $1 \
                 ORDER BY merged ASC, pr_number DESC",
                &[&claude_session_id],
            )
            .await
            .map_err(|e| format!("PG list_session_prs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| SessionPrRow {
                claude_session_id: r.get(0),
                repo: r.get(1),
                pr_number: r.get(2),
                head_branch: r.get(3),
                pr_state: r.get(4),
                merged: r.get(5),
                merged_at: r.get(6),
                land_signal: r.get(7),
                land_reason: r.get(8),
                landed_at: r.get(9),
                created_at: r.get(10),
            })
            .collect())
    }

    /// Refresh only the lifecycle + land columns of an already-attributed PR —
    /// the Phase-3 status-tracking write. A no-op (zero rows affected, no
    /// error) if the row is absent, so the reconciler can call it
    /// unconditionally after a GitHub status fetch.
    ///
    /// Carries the same land monotonicity as [`Self::upsert_session_pr`]: this
    /// is the path that runs AFTER the local branch is gone, i.e. exactly when
    /// the head object is most likely to have been pruned.
    pub async fn update_session_pr_status(
        &self,
        claude_session_id: Uuid,
        repo: &str,
        pr_number: i64,
        status: &SessionPrStatus,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE project.session_prs SET \
                 pr_state    = CASE WHEN merged AND NOT $5 THEN pr_state    ELSE $4 END, \
                 merged      = merged OR $5, \
                 merged_at   = COALESCE($6, merged_at), \
                 land_signal = CASE WHEN merged AND NOT $5 THEN land_signal ELSE $7 END, \
                 land_reason = CASE WHEN merged AND NOT $5 THEN land_reason ELSE $8 END, \
                 landed_at   = CASE WHEN merged AND NOT $5 THEN landed_at \
                                    ELSE COALESCE($9, landed_at) END, \
                 updated_at  = now() \
             WHERE claude_session_id = $1 AND repo = $2 AND pr_number = $3",
            &[
                &claude_session_id,
                &repo,
                &pr_number,
                &status.pr_state,
                &status.landed,
                &status.merged_at,
                &status.land_signal,
                &status.land_reason,
                &status.landed_at,
            ],
        )
        .await
        .map_err(|e| format!("PG update_session_pr_status: {}", e))?;

        Ok(())
    }
}
