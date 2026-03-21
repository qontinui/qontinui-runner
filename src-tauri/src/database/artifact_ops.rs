//! Artifact persistence operations.
//!
//! Contains all CheckpointDb methods related to UI Bridge IPC artifact storage.

use rusqlite::params;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Artifacts (UI Bridge IPC)
    // ========================================================================

    /// Insert a new artifact. Returns an error if the artifact_id already exists.
    pub fn save_artifact(&self, artifact: &Artifact) -> Result<(), String> {
        let conn = self.get_conn_string()?;

        let passed_int: Option<i32> = artifact.passed.map(|b| if b { 1 } else { 0 });

        conn.execute(
            r#"
            INSERT INTO artifacts (artifact_id, source_json, result_json, environment_json, created_at, passed)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                artifact.artifact_id,
                artifact.source_json,
                artifact.result_json,
                artifact.environment_json,
                artifact.created_at,
                passed_int,
            ],
        )
        .map_err(|e| {
            if let rusqlite::Error::SqliteFailure(ref err, _) = e {
                if err.code == rusqlite::ErrorCode::ConstraintViolation {
                    return format!(
                        "Artifact with id '{}' already exists",
                        artifact.artifact_id
                    );
                }
            }
            format!("Failed to save artifact: {}", e)
        })?;

        Ok(())
    }

    /// Get a single artifact by its ID.
    pub fn get_artifact(&self, artifact_id: &str) -> Result<Option<Artifact>, String> {
        let conn = self.get_conn_string()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT artifact_id, source_json, result_json, environment_json, created_at, passed
                FROM artifacts
                WHERE artifact_id = ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let mut rows = stmt
            .query_map(params![artifact_id], |row| {
                let passed_int: Option<i32> = row.get(5)?;
                Ok(Artifact {
                    artifact_id: row.get(0)?,
                    source_json: row.get(1)?,
                    result_json: row.get(2)?,
                    environment_json: row.get(3)?,
                    created_at: row.get(4)?,
                    passed: passed_int.map(|v| v != 0),
                })
            })
            .map_err(|e| format!("Failed to query artifact: {}", e))?;

        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| format!("Failed to read row: {}", e))?)),
            None => Ok(None),
        }
    }

    /// Query artifacts with optional filters, ordered by created_at DESC.
    pub fn query_artifacts(&self, query: &ArtifactQuery) -> Result<Vec<Artifact>, String> {
        let conn = self.get_conn_string()?;

        let mut sql = String::from(
            "SELECT artifact_id, source_json, result_json, environment_json, created_at, passed FROM artifacts WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(ref spec_id) = query.spec_id {
            sql.push_str(&format!(
                " AND json_extract(source_json, '$.specId') = ?{}",
                param_idx
            ));
            param_values.push(Box::new(spec_id.clone()));
            param_idx += 1;
        }

        if let Some(ref date_from) = query.date_from {
            sql.push_str(&format!(" AND created_at >= ?{}", param_idx));
            param_values.push(Box::new(date_from.clone()));
            param_idx += 1;
        }

        if let Some(ref date_to) = query.date_to {
            sql.push_str(&format!(" AND created_at <= ?{}", param_idx));
            param_values.push(Box::new(date_to.clone()));
            param_idx += 1;
        }

        if query.passed_only == Some(true) {
            sql.push_str(&format!(" AND passed = ?{}", param_idx));
            param_values.push(Box::new(1i32));
            param_idx += 1;
        } else if query.failed_only == Some(true) {
            sql.push_str(&format!(" AND passed = ?{}", param_idx));
            param_values.push(Box::new(0i32));
            param_idx += 1;
        }

        sql.push_str(" ORDER BY created_at DESC");

        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT ?{}", param_idx));
            param_values.push(Box::new(limit as i64));
            param_idx += 1;
        }

        if let Some(offset) = query.offset {
            sql.push_str(&format!(" OFFSET ?{}", param_idx));
            param_values.push(Box::new(offset as i64));
            // param_idx not needed after this
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let passed_int: Option<i32> = row.get(5)?;
                Ok(Artifact {
                    artifact_id: row.get(0)?,
                    source_json: row.get(1)?,
                    result_json: row.get(2)?,
                    environment_json: row.get(3)?,
                    created_at: row.get(4)?,
                    passed: passed_int.map(|v| v != 0),
                })
            })
            .map_err(|e| format!("Failed to query artifacts: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Count artifacts with optional filters.
    pub fn count_artifacts(&self, query: &ArtifactCountQuery) -> Result<i64, String> {
        let conn = self.get_conn_string()?;

        let mut sql = String::from("SELECT COUNT(*) FROM artifacts WHERE 1=1");
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(ref spec_id) = query.spec_id {
            sql.push_str(&format!(
                " AND json_extract(source_json, '$.specId') = ?{}",
                param_idx
            ));
            param_values.push(Box::new(spec_id.clone()));
            param_idx += 1;
        }

        if let Some(ref date_from) = query.date_from {
            sql.push_str(&format!(" AND created_at >= ?{}", param_idx));
            param_values.push(Box::new(date_from.clone()));
            param_idx += 1;
        }

        if let Some(ref date_to) = query.date_to {
            sql.push_str(&format!(" AND created_at <= ?{}", param_idx));
            param_values.push(Box::new(date_to.clone()));
            param_idx += 1;
        }

        if query.passed_only == Some(true) {
            sql.push_str(&format!(" AND passed = ?{}", param_idx));
            param_values.push(Box::new(1i32));
            param_idx += 1;
        } else if query.failed_only == Some(true) {
            sql.push_str(&format!(" AND passed = ?{}", param_idx));
            param_values.push(Box::new(0i32));
            // param_idx not needed after this
        }

        let _ = param_idx; // suppress unused warning

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))
            .map_err(|e| format!("Failed to count artifacts: {}", e))
    }
}
