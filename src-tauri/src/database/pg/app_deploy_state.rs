//! PostgreSQL CRUD for `project.app_deploy_state` — P3 auto-fresh app deployment tracking.
//!
//! The table is authored declaratively in `atlas/schema.hcl` AND mirrored as
//! a `CREATE TABLE IF NOT EXISTS` self-heal in `pg/mod.rs::PgDb::new` so a
//! fresh PG without Atlas applied still boots. Query style follows
//! `pg/apps.rs` — direct `tokio_postgres`, no Clorinde codegen.
//!
//! `update_app_deploy_state` is an UPSERT that records the deployment outcome
//! (fresh SHA, error, freshness status) per app per device. Best-effort: any
//! database errors are logged at `warn!` but never returned to callers, because
//! the calling code path is in the fleet background loop and a flaky UPDATE
//! must not poison it.

use std::sync::Arc;
use tracing::warn;

use super::PgDb;

/// UPSERT the deployment state for one app on one device. Captures the outcome
/// of an auto-fresh pull attempt (successful SHA, error message, freshness flag).
///
/// - `device_id` (uuid): the device pulling the app
/// - `app_id` (string): the app being pulled (e.g. "qontinui-runner", "qontinui-web")
/// - `deployed_sha` (Option<String>): the new HEAD SHA after a successful pull, or None on failure
/// - `freshness` (string): "fresh" on success, "failed" on error
/// - `last_error` (Option<String>): error message if present (None on success)
/// - `updated_at` (now): current timestamp
///
/// Idempotent — safe to call repeatedly for the same (device_id, app_id) pair.
/// Returns Ok on success, Err(msg) if database access fails. Errors are logged
/// at warn! and never propagated to callers — the auto-fresh cycle is best-effort.
pub async fn update_app_deploy_state(
    pg: &Arc<PgDb>,
    device_id: uuid::Uuid,
    app_id: &str,
    deployed_sha: Option<&str>,
    freshness: &str,
    last_error: Option<&str>,
) -> Result<(), String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let now = chrono::Utc::now();

    conn.execute(
        "INSERT INTO project.app_deploy_state \
               (device_id, app_id, deployed_sha, freshness, last_error, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (device_id, app_id) DO UPDATE SET \
             deployed_sha = EXCLUDED.deployed_sha, \
             freshness = EXCLUDED.freshness, \
             last_error = EXCLUDED.last_error, \
             updated_at = EXCLUDED.updated_at",
        &[
            &device_id as &(dyn tokio_postgres::types::ToSql + Sync),
            &app_id,
            &deployed_sha as &(dyn tokio_postgres::types::ToSql + Sync),
            &freshness,
            &last_error as &(dyn tokio_postgres::types::ToSql + Sync),
            &now,
        ],
    )
    .await
    .map_err(|e| format!("PG app_deploy_state upsert: {}", e))?;

    Ok(())
}

/// UPSERT the deployment state with best-effort semantics — errors are logged
/// but never propagated. Wrapper around `update_app_deploy_state` for the
/// auto-fresh cycle that cannot fail the cycle on database errors.
pub async fn update_app_deploy_state_best_effort(
    pg: &Arc<PgDb>,
    device_id: uuid::Uuid,
    app_id: &str,
    deployed_sha: Option<&str>,
    freshness: &str,
    last_error: Option<&str>,
) {
    if let Err(e) = update_app_deploy_state(pg, device_id, app_id, deployed_sha, freshness, last_error).await {
        warn!(
            "fleet::auto_fresh: app_deploy_state upsert failed for {app_id} on {device_id}: {e}"
        );
    }
}
