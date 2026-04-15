//! PostgreSQL memory query cache operations.
//!
//! Caches expensive LLM-synthesized results from High/Max reasoning tiers.

use super::PgDb;

impl PgDb {
    /// Look up a cached query result by hash and reasoning level.
    /// Returns the cached `result_json` if found and not expired.
    pub async fn get_cached_query(
        &self,
        query_hash: &str,
        reasoning_level: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "UPDATE memory_query_cache
                 SET hit_count = hit_count + 1
                 WHERE query_hash = $1 AND reasoning_level = $2 AND expires_at > NOW()
                 RETURNING result_json",
                &[&query_hash, &reasoning_level],
            )
            .await
            .map_err(|e| format!("PG get_cached_query: {}", e))?;

        Ok(row.map(|r| r.get("result_json")))
    }

    /// Save a query result to the cache with a TTL in minutes.
    pub async fn save_cached_query(
        &self,
        query_hash: &str,
        reasoning_level: &str,
        result_json: &str,
        ttl_minutes: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "INSERT INTO memory_query_cache (query_hash, reasoning_level, result_json, expires_at)
             VALUES ($1, $2, $3, NOW() + ($4 || ' minutes')::INTERVAL)
             ON CONFLICT (query_hash, reasoning_level)
             DO UPDATE SET result_json = EXCLUDED.result_json,
                           hit_count = 0,
                           created_at = NOW(),
                           expires_at = EXCLUDED.expires_at",
            &[
                &query_hash,
                &reasoning_level,
                &result_json,
                &ttl_minutes.to_string(),
            ],
        )
        .await
        .map_err(|e| format!("PG save_cached_query: {}", e))?;

        Ok(())
    }

    /// Delete all expired cache entries. Returns the number of rows removed.
    pub async fn cleanup_expired_cache(&self) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = conn
            .execute(
                "DELETE FROM memory_query_cache WHERE expires_at <= NOW()",
                &[],
            )
            .await
            .map_err(|e| format!("PG cleanup_expired_cache: {}", e))?;

        Ok(deleted)
    }
}
