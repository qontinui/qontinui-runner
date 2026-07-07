//! Database operations for the PR-shepherd remediation dedup marker
//! (`project.pr_shepherd_remediations`) — Phase 5 of plan
//! `2026-07-04-runner-pr-shepherd.md`.
//!
//! One row per remediation the device-local shepherd authored. The row is the
//! durable, cross-restart arbiter for two dedup concerns:
//!
//! - **Cooldown:** skip a fresh remediation for the same defect class within
//!   `QONTINUI_PR_SHEPHERD_REMEDIATION_COOLDOWN` (default 24h).
//! - **Steward coexistence:** the marker records that *some* remediator (this
//!   daemon) already acted, complementing the plans-dir scan that also catches
//!   the fleet-wide merge-train-steward's plans.

use super::PgDb;

impl PgDb {
    /// Is there a `pr_shepherd_remediations` row for `defect_class` younger than
    /// `cooldown_secs`? Used as the Phase 5 cooldown dedup gate BEFORE writing a
    /// plan / spawning a session.
    pub async fn recent_pr_shepherd_remediation(
        &self,
        defect_class: &str,
        cooldown_secs: i64,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT 1 FROM project.pr_shepherd_remediations \
                 WHERE defect_class = $1 \
                   AND created_at > now() - make_interval(secs => $2::double precision) \
                 LIMIT 1",
                &[&defect_class, &(cooldown_secs as f64)],
            )
            .await
            .map_err(|e| format!("PG recent_pr_shepherd_remediation: {}", e))?;

        Ok(row.is_some())
    }

    /// Insert a remediation marker. `spawned_session` is `None` when spawn was
    /// disabled (plan written, no session). Returns the new row id.
    pub async fn insert_pr_shepherd_remediation(
        &self,
        defect_class: &str,
        pr: u64,
        plan_path: &str,
        spawned_session: Option<&str>,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let pr_i = pr as i64;
        conn.execute(
            "INSERT INTO project.pr_shepherd_remediations \
                 (id, defect_class, pr, plan_path, spawned_session) \
             VALUES ($1, $2, $3, $4, $5)",
            &[&id, &defect_class, &pr_i, &plan_path, &spawned_session],
        )
        .await
        .map_err(|e| format!("PG insert_pr_shepherd_remediation: {}", e))?;

        Ok(id)
    }

    /// Attach the spawned child TaskRun id to an existing marker row (called
    /// after a successful spawn when the row was inserted before spawning).
    pub async fn set_pr_shepherd_remediation_session(
        &self,
        id: &str,
        spawned_session: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE project.pr_shepherd_remediations SET spawned_session = $1 WHERE id = $2",
            &[&spawned_session, &id],
        )
        .await
        .map_err(|e| format!("PG set_pr_shepherd_remediation_session: {}", e))?;

        Ok(())
    }
}
