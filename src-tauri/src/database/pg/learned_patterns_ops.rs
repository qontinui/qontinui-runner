//! PostgreSQL ops for pattern distillation: reading finding/fix pairs from the
//! knowledge graph and persisting `learned_patterns` entries.

use sha2::{Digest, Sha256};

use crate::database::pg::PgDb;
use crate::online_learning::pattern_distiller::LearnedPattern;

/// Finding/fix pair from the knowledge graph.
pub struct FindingFixPair {
    pub finding_description: String,
    pub fix_description: String,
    pub fix_applied_successfully: bool,
    pub task_run_id: String,
}

impl PgDb {
    /// Query finding→fix pairs from successful runs of a workflow.
    ///
    /// Joins `reflection_fixes` (which represent applied fixes) with the
    /// originating `task_run_findings` and filters by completed runs of the
    /// given workflow. Gracefully returns an empty vec if the tables are
    /// missing (schema not yet migrated).
    pub async fn get_finding_fix_pairs_for_workflow(
        &self,
        workflow_name: &str,
    ) -> Result<Vec<FindingFixPair>, String> {
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => return Err(format!("PG pool error: {}", e)),
        };

        // Join: reflection_fixes → task_run_findings (finding) → task_runs (workflow filter).
        // The reflection_fixes.effectiveness column is NULL by default; the COALESCE
        // below replaces NULL with 'effective', so downstream we only need to match
        // on the literal string values.
        let sql = r#"
            SELECT trf.description AS finding_description,
                   rf.fix_description AS fix_description,
                   COALESCE(rf.effectiveness, 'effective') AS effectiveness,
                   rf.source_task_run_id AS task_run_id
            FROM reflection_fixes rf
            JOIN task_run_findings trf ON trf.id = rf.source_finding_id
            JOIN task_runs tr ON tr.id = rf.source_task_run_id
            WHERE tr.workflow_name = $1
              AND tr.status IN ('complete', 'completed', 'success')
              AND rf.source_finding_id IS NOT NULL
            LIMIT 1000
        "#;

        let rows = match conn.query(sql, &[&workflow_name]).await {
            Ok(r) => r,
            Err(e) => {
                // Graceful degradation — tables/columns may not exist yet.
                tracing::debug!(
                    "get_finding_fix_pairs_for_workflow: query failed ({}) — returning empty",
                    e
                );
                return Ok(Vec::new());
            }
        };

        let mut pairs = Vec::with_capacity(rows.len());
        for row in &rows {
            let finding_description: String = row.get(0);
            let fix_description: String = row.get(1);
            let effectiveness: String = row.get(2);
            let task_run_id: String = row.get(3);
            // COALESCE in the SQL above guarantees `effectiveness` is non-NULL;
            // 'effective' = success, anything else (e.g. 'ineffective',
            // 'caused_regression') counts as a failure.
            let fix_applied_successfully = effectiveness == "effective";
            pairs.push(FindingFixPair {
                finding_description,
                fix_description,
                fix_applied_successfully,
                task_run_id,
            });
        }
        Ok(pairs)
    }

    /// Upsert a learned pattern, deduping by SHA-256 hash of the normalized
    /// problem description. On conflict, confidence and sample_count are
    /// replaced with the latest computed values.
    pub async fn upsert_learned_pattern(&self, pattern: &LearnedPattern) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let problem_hash = {
            let mut hasher = Sha256::new();
            hasher.update(pattern.problem_description.to_lowercase().as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let keywords_json: serde_json::Value = serde_json::to_value(&pattern.trigger_keywords)
            .map_err(|e| format!("PG upsert_learned_pattern serialize: {}", e))?;
        let sample_count: i32 = pattern.sample_count as i32;

        let sql = r#"
            INSERT INTO learned_patterns (
                id, problem_hash, trigger_keywords, problem_description,
                solution_description, confidence, sample_count, project_path, workflow_name
            )
            VALUES ($1, $2, $3::jsonb, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (problem_hash) DO UPDATE SET
                trigger_keywords     = EXCLUDED.trigger_keywords,
                solution_description = EXCLUDED.solution_description,
                confidence           = EXCLUDED.confidence,
                sample_count         = EXCLUDED.sample_count,
                project_path         = EXCLUDED.project_path,
                workflow_name        = EXCLUDED.workflow_name,
                updated_at           = NOW()
        "#;

        conn.execute(
            sql,
            &[
                &pattern.id,
                &problem_hash,
                &keywords_json,
                &pattern.problem_description,
                &pattern.solution_description,
                &pattern.confidence,
                &sample_count,
                &pattern.project_path,
                &pattern.workflow_name,
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_learned_pattern: {}", e))?;

        Ok(())
    }

    /// Get learned patterns, optionally filtered by project path. Patterns
    /// with a NULL project_path are considered global and always returned.
    pub async fn get_learned_patterns(
        &self,
        project_path: Option<&str>,
    ) -> Result<Vec<LearnedPattern>, String> {
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => return Err(format!("PG pool error: {}", e)),
        };

        let sql = r#"
            SELECT id, trigger_keywords, problem_description, solution_description,
                   confidence, sample_count, project_path, workflow_name
            FROM learned_patterns
            WHERE project_path IS NULL OR project_path = $1
            ORDER BY confidence DESC
            LIMIT 100
        "#;

        let project_path_param: Option<String> = project_path.map(|s| s.to_string());

        let rows = match conn.query(sql, &[&project_path_param]).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    "get_learned_patterns: query failed ({}) — returning empty",
                    e
                );
                return Ok(Vec::new());
            }
        };

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.get(0);
            let keywords_value: serde_json::Value = row.get(1);
            let problem_description: String = row.get(2);
            let solution_description: String = row.get(3);
            let confidence: f64 = row.get(4);
            let sample_count: i32 = row.get(5);
            let project_path: Option<String> = row.get(6);
            let workflow_name: Option<String> = row.get(7);

            let trigger_keywords: Vec<String> =
                serde_json::from_value(keywords_value).unwrap_or_default();

            out.push(LearnedPattern {
                id,
                trigger_keywords,
                problem_description,
                solution_description,
                confidence,
                sample_count: sample_count.max(0) as u32,
                project_path,
                workflow_name,
            });
        }
        Ok(out)
    }
}
