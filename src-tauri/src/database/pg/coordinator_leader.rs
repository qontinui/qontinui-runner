//! PostgreSQL CRUD for the `coord.coordinator_leader` singleton (migration v29).
//!
//! Per productivity-stack §6 ("Multi-instance awareness"), only one
//! `/coordinate` session may run at a time across the runner's PG-shared
//! cluster. The lease is a single PG row gated by
//! `id BOOLEAN PRIMARY KEY DEFAULT TRUE` (same idiom used by
//! `scheduler_settings`), so two coordinators can't both believe they own
//! the queue.
//!
//! ## Lifecycle
//!
//! - `try_acquire_lease(instance_id, ttl)` — succeeds when no row exists,
//!   the existing row's lease has expired (`leased_until <= NOW()`), or the
//!   caller already holds the lease (idempotent renewal).
//! - `renew_lease(instance_id, ttl)` — extends the lease only if the caller
//!   already owns it. Returns `false` if another instance has stolen it.
//! - `current_leader()` — read-only inspection for the dashboard.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// Snapshot of the leader-lease row — `None` when no instance has ever
/// taken the lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorLeaderRow {
    pub instance_id: String,
    pub leased_until: String,
    pub acquired_at: String,
    pub renewed_at: String,
}

impl PgDb {
    /// Try to acquire the leader lease for `instance_id` with a TTL of
    /// `ttl_seconds`. Returns `true` if the lease is now held by
    /// `instance_id`, `false` if another instance still owns a non-expired
    /// lease.
    ///
    /// Idempotent: calling repeatedly with the same `instance_id` extends
    /// the existing lease. Equivalent to `renew_lease` in that case but
    /// safer to call from startup paths where we don't yet know whether
    /// we're the incumbent.
    pub async fn try_acquire_coordinator_lease(
        &self,
        instance_id: &str,
        ttl_seconds: i64,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // INSERT ... ON CONFLICT updates the row only when the existing
        // lease has expired OR the caller already holds it. The CASE
        // expression keeps `acquired_at` if the same instance is renewing,
        // and resets it on a true takeover.
        let n = conn
            .execute(
                r#"
                INSERT INTO coord.coordinator_leader (id, instance_id, leased_until, acquired_at, renewed_at)
                VALUES (TRUE, $1, NOW() + ($2::bigint || ' seconds')::interval, NOW(), NOW())
                ON CONFLICT (id) DO UPDATE
                    SET instance_id   = EXCLUDED.instance_id,
                        leased_until  = EXCLUDED.leased_until,
                        acquired_at   = CASE
                            WHEN coord.coordinator_leader.instance_id = EXCLUDED.instance_id
                                THEN coord.coordinator_leader.acquired_at
                            ELSE NOW()
                        END,
                        renewed_at    = NOW()
                    WHERE coord.coordinator_leader.leased_until <= NOW()
                       OR coord.coordinator_leader.instance_id = EXCLUDED.instance_id
                "#,
                &[&instance_id, &ttl_seconds],
            )
            .await
            .map_err(|e| format!("Failed to acquire coordinator lease: {}", e))?;

        Ok(n > 0)
    }

    /// Renew an existing lease. Returns `true` if `instance_id` still owned
    /// the row and the TTL was extended, `false` if some other instance
    /// has stolen the lease in the meantime.
    pub async fn renew_coordinator_lease(
        &self,
        instance_id: &str,
        ttl_seconds: i64,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.coordinator_leader
                SET leased_until = NOW() + ($2::bigint || ' seconds')::interval,
                    renewed_at   = NOW()
                WHERE id = TRUE
                  AND instance_id = $1
                "#,
                &[&instance_id, &ttl_seconds],
            )
            .await
            .map_err(|e| format!("Failed to renew coordinator lease: {}", e))?;

        Ok(n > 0)
    }

    /// Clear the leader-lease row only if the lease has been stale for at
    /// least 60s (`leased_until <= NOW() - INTERVAL '60 seconds'`). Returns
    /// `Ok(true)` when a stale row was cleared, `Ok(false)` if no row was
    /// matched. The 60s grace prevents racing with normal renewal cadence
    /// (lease TTL is 60s, renew interval is shorter) — calling this on a
    /// healthy lease is therefore an idempotent no-op.
    pub async fn release_stale_coordinator_lease(&self) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                DELETE FROM coord.coordinator_leader
                WHERE id = TRUE
                  AND leased_until <= NOW() - INTERVAL '60 seconds'
                RETURNING instance_id
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to release stale coordinator lease: {}", e))?;

        Ok(row.is_some())
    }

    /// Return the current leader-lease row, or `None` if no instance has
    /// taken the lease yet. Read-only — intended for the dashboard's
    /// "current leader" badge.
    pub async fn current_coordinator_leader(&self) -> Result<Option<CoordinatorLeaderRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT instance_id,
                       leased_until::text,
                       acquired_at::text,
                       renewed_at::text
                FROM coord.coordinator_leader
                WHERE id = TRUE
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to load coordinator leader: {}", e))?;

        Ok(row.map(|r| CoordinatorLeaderRow {
            instance_id: r.get(0),
            leased_until: r.get(1),
            acquired_at: r.get(2),
            renewed_at: r.get(3),
        }))
    }
}
