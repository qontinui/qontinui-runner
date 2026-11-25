//! The embedded-PG schema "version" an upgrade records.
//!
//! The runner's local database has no migration ledger: its tables are made by
//! idempotent `CREATE … IF NOT EXISTS` self-provisioning (`database::pg`) plus
//! the Atlas-owned `atlas_managed` schema. So there is no version NUMBER to
//! record. What the rollback rule needs is narrower — "did the successor change
//! the schema the old build reads?" — and a fingerprint of the column catalogue
//! answers exactly that: every `(schema, table, column, type)` in the runner's
//! schemas, hashed in a fixed order. Two equal fingerprints mean an identical
//! catalogue; a different one means the successor added, dropped or retyped
//! something, and the guard refuses to roll back across it (plan Risks:
//! "Rollback across a coord/DB migration").

use std::time::Duration;

/// The schemas the runner owns.
pub const RUNNER_SCHEMAS: &[&str] = &["project", "runner", "orchestration", "atlas_managed"];

/// The catalogue read. Ordered, so the hash is deterministic.
const FINGERPRINT_SQL: &str = "SELECT count(*)::bigint, \
     coalesce(md5(string_agg(table_schema::text || '.' || table_name::text || '.' || column_name::text \
       || ':' || data_type::text, ',' ORDER BY table_schema, table_name, column_name)), '') \
     FROM information_schema.columns WHERE table_schema::text = ANY($1::text[])";

const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `md5:<hex>/<column count>` for the running database, or why it could not be
/// read (no database configured, a timeout, a query error). An `Err` is
/// UNKNOWN, never "no schema".
pub async fn fingerprint() -> Result<String, String> {
    let pg = crate::database::pg::PgDb::try_global()
        .ok_or_else(|| "no PostgreSQL configured in this runner".to_string())?;
    let schemas: Vec<String> = RUNNER_SCHEMAS.iter().map(|s| (*s).to_string()).collect();
    let read = async move {
        let conn = pg.pool().get().await.map_err(|e| e.to_string())?;
        let row = conn
            .query_one(FINGERPRINT_SQL, &[&schemas])
            .await
            .map_err(|e| e.to_string())?;
        let count: i64 = row.try_get(0).map_err(|e| e.to_string())?;
        let hash: String = row.try_get(1).map_err(|e| e.to_string())?;
        Ok::<_, String>(format_fingerprint(&hash, count))
    };
    match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(r) => r.map_err(|e| format!("embedded-PG schema read failed: {e}")),
        Err(_) => Err(format!(
            "embedded-PG schema read timed out after {}s",
            READ_TIMEOUT.as_secs()
        )),
    }
}

/// Which database arm this process took (embedded owned / attached /
/// external / degraded), for the record's detail line.
pub fn db_arm_label() -> String {
    format!("{:?}", crate::embedded_pg::db_arm())
}

fn format_fingerprint(hash: &str, columns: i64) -> String {
    format!("md5:{hash}/{columns}")
}

/// Does the successor's schema differ from the old build's? `None` when either
/// side was not recorded — the caller decides what an unknown means.
pub fn changed(from: Option<&str>, to: Option<&str>) -> Option<bool> {
    Some(from? != to?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_is_unknown_unless_both_sides_were_recorded() {
        assert_eq!(changed(Some("md5:a/3"), Some("md5:a/3")), Some(false));
        assert_eq!(changed(Some("md5:a/3"), Some("md5:b/4")), Some(true));
        assert_eq!(changed(Some("md5:a/3"), None), None);
        assert_eq!(changed(None, Some("md5:a/3")), None);
    }

    #[test]
    fn fingerprint_names_the_hash_and_the_column_count() {
        assert_eq!(format_fingerprint("abc", 12), "md5:abc/12");
        assert!(FINGERPRINT_SQL.contains("ORDER BY table_schema, table_name, column_name"));
    }
}
