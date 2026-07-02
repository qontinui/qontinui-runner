//! App deployment state tracking for fleet-fresh auto-fresh engine (P3 follow-on).
//!
//! Records deployment outcomes: deployed_sha, freshness (fresh/building/failed),
//! and error messages. Used by P3 to update state after pull+build+restart,
//! and by P4 dispatcher to route tests to fresh hosts.

use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use super::PgDb;

impl PgDb {
    /// UPSERT deployment state per (device_id, app_id) with ON CONFLICT logic.
    ///
    /// Used by P3 auto-fresh engine to record outcomes:
    /// - On success: freshness='fresh', deployed_sha=new_sha, last_error=None
    /// - On failure: freshness='failed', deployed_sha=None, last_error=error_msg
    /// - While building: freshness='building', deployed_sha=None
    pub async fn update_app_deploy_state(
        &self,
        device_id: Uuid,
        app_id: &str,
        deployed_sha: Option<&str>,
        freshness: &str,
        last_error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = Utc::now();

        conn.execute(
            "INSERT INTO project.app_deploy_state \
             (device_id, app_id, deployed_sha, freshness, last_error, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (device_id, app_id) \
             DO UPDATE SET \
                 deployed_sha = EXCLUDED.deployed_sha, \
                 freshness = EXCLUDED.freshness, \
                 last_error = EXCLUDED.last_error, \
                 updated_at = EXCLUDED.updated_at",
            &[
                &device_id,
                &app_id,
                &deployed_sha,
                &freshness,
                &last_error,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG update_app_deploy_state: {}", e))?;

        Ok(())
    }

    /// Best-effort wrapper for update_app_deploy_state (logs errors, never propagates).
    ///
    /// Used in P3 cycle to record state without breaking the auto-fresh loop.
    pub async fn update_app_deploy_state_best_effort(
        &self,
        device_id: Uuid,
        app_id: &str,
        deployed_sha: Option<&str>,
        freshness: &str,
        last_error: Option<&str>,
    ) {
        if let Err(e) = self
            .update_app_deploy_state(device_id, app_id, deployed_sha, freshness, last_error)
            .await
        {
            warn!(
                "fleet::auto_fresh_engine: failed to update app_deploy_state \
                 device_id={} app_id={}: {}",
                device_id, app_id, e
            );
        }
    }
}
