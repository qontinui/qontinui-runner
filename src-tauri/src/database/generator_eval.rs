//! Generator evaluation operations: pipeline artifacts, dashboard metrics,
//! benchmarks, edit analysis, and example library.
//!
//! Contains all CheckpointDb methods related to generator evaluation.

use rusqlite::params;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Generator Evaluation - Pipeline Artifacts
    // ========================================================================

    /// Save a pipeline artifact from a generation run.
    pub fn save_pipeline_artifact(
        &self,
        artifact: &crate::workflow_generation::pipeline_artifacts::PipelineArtifact,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generation_pipeline_artifacts
                (id, workflow_id, task_run_id, description, category, created_at,
                 investigation_duration_ms, investigation_enriched_description,
                 specification_duration_ms, specification_criteria,
                 specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                 discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                 verification_duration_ms, hardener_duration_ms, total_duration_ms,
                 discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                 verification_iterations, fixer_snapshots, hardening_summary,
                 hardened_json, final_json, validation_errors,
                 success, error_message, model_used,
                 revision_duration_ms, quality_report, revision_cycles,
                 confidence_score)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                    ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37)"#,
            params![
                artifact.id,
                artifact.workflow_id,
                artifact.task_run_id,
                artifact.description,
                artifact.category,
                artifact.created_at,
                artifact.investigation_duration_ms.map(|v| v as i64),
                artifact.investigation_enriched_description,
                artifact.specification_duration_ms.map(|v| v as i64),
                artifact
                    .specification_criteria
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact.specification_prompt,
                artifact.builder_prompt,
                artifact
                    .verification_prompts
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact.hardener_prompt,
                artifact.discovery_duration_ms.map(|v| v as i64),
                artifact.builder_duration_ms.map(|v| v as i64),
                artifact.autofix_duration_ms.map(|v| v as i64),
                artifact.verification_duration_ms.map(|v| v as i64),
                artifact.hardener_duration_ms.map(|v| v as i64),
                artifact.total_duration_ms.map(|v| v as i64),
                artifact.discovery_calls.as_ref().map(|v| v.to_string()),
                artifact.builder_raw_output,
                artifact.builder_parsed_json.as_ref().map(|v| v.to_string()),
                artifact.autofix_diff.as_ref().map(|v| v.to_string()),
                artifact
                    .verification_iterations
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact
                    .fixer_snapshots
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
                artifact.hardening_summary.as_ref().map(|v| v.to_string()),
                artifact.hardened_json.as_ref().map(|v| v.to_string()),
                artifact.final_json.as_ref().map(|v| v.to_string()),
                artifact.validation_errors.as_ref().map(|v| v.to_string()),
                artifact.success,
                artifact.error_message,
                artifact.model_used,
                artifact.revision_duration_ms.map(|v| v as i64),
                artifact.quality_report.as_ref().map(|v| v.to_string()),
                artifact.revision_cycles.map(|v| v as i32),
                artifact.confidence_score.map(|v| v as f64),
            ],
        )
        .map_err(|e| format!("Failed to save pipeline artifact: {}", e))?;
        Ok(())
    }

    /// Get a pipeline artifact by ID.
    pub fn get_pipeline_artifact(
        &self,
        id: &str,
    ) -> Result<Option<crate::workflow_generation::pipeline_artifacts::PipelineArtifact>, String>
    {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, workflow_id, task_run_id, description, category, created_at,
                      investigation_duration_ms, investigation_enriched_description,
                      specification_duration_ms, specification_criteria,
                      specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                      discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                      verification_duration_ms, hardener_duration_ms, total_duration_ms,
                      discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                      verification_iterations, fixer_snapshots, hardening_summary,
                      hardened_json, final_json, validation_errors,
                      success, error_message, model_used,
                      revision_duration_ms, quality_report, revision_cycles,
                      confidence_score
               FROM generation_pipeline_artifacts WHERE id = ?1"#,
            params![id],
            |row| Ok(Self::row_to_pipeline_artifact(row)),
        );
        match result {
            Ok(artifact) => Ok(Some(artifact)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get pipeline artifact: {}", e)),
        }
    }

    /// List pipeline artifacts (paginated, newest first).
    pub fn list_pipeline_artifacts(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<crate::workflow_generation::pipeline_artifacts::PipelineArtifactSummary>, String>
    {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, workflow_id, description, category, created_at,
                          total_duration_ms, success, model_used,
                          verification_iterations, hardening_summary
                   FROM generation_pipeline_artifacts
                   ORDER BY created_at DESC
                   LIMIT ?1 OFFSET ?2"#,
            )
            .map_err(|e| format!("Failed to prepare list query: {}", e))?;

        let rows = stmt
            .query_map(params![limit, offset], |row| {
                let verification_json: Option<String> = row.get(8)?;
                let hardening_json: Option<String> = row.get(9)?;

                let verification_count = verification_json
                    .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
                    .map(|v| v.len() as u32)
                    .unwrap_or(0);

                let hardener_count = hardening_json
                    .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                    .and_then(|v| v.get("converted_count")?.as_u64())
                    .unwrap_or(0) as u32;

                Ok(
                    crate::workflow_generation::pipeline_artifacts::PipelineArtifactSummary {
                        id: row.get(0)?,
                        workflow_id: row.get(1)?,
                        description: row.get(2)?,
                        category: row.get(3)?,
                        created_at: row.get(4)?,
                        total_duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                        success: row.get(6)?,
                        model_used: row.get(7)?,
                        verification_iteration_count: verification_count,
                        hardener_converted_count: hardener_count,
                    },
                )
            })
            .map_err(|e| format!("Failed to list pipeline artifacts: {}", e))?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(artifacts)
    }

    /// Get pipeline artifact for a specific workflow.
    pub fn get_pipeline_artifact_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<crate::workflow_generation::pipeline_artifacts::PipelineArtifact>, String>
    {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, workflow_id, task_run_id, description, category, created_at,
                      investigation_duration_ms, investigation_enriched_description,
                      specification_duration_ms, specification_criteria,
                      specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                      discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                      verification_duration_ms, hardener_duration_ms, total_duration_ms,
                      discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                      verification_iterations, fixer_snapshots, hardening_summary,
                      hardened_json, final_json, validation_errors,
                      success, error_message, model_used,
                      revision_duration_ms, quality_report, revision_cycles,
                      confidence_score
               FROM generation_pipeline_artifacts
               WHERE workflow_id = ?1
               ORDER BY created_at DESC LIMIT 1"#,
            params![workflow_id],
            |row| Ok(Self::row_to_pipeline_artifact(row)),
        );
        match result {
            Ok(artifact) => Ok(Some(artifact)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get artifact for workflow: {}", e)),
        }
    }

    /// Delete pipeline artifacts older than N days.
    pub fn delete_pipeline_artifacts_older_than(&self, days: u32) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM generation_pipeline_artifacts WHERE created_at < datetime('now', ?1)",
                params![format!("-{} days", days)],
            )
            .map_err(|e| format!("Failed to prune artifacts: {}", e))?;
        Ok(deleted as u32)
    }

    fn row_to_pipeline_artifact(
        row: &rusqlite::Row,
    ) -> crate::workflow_generation::pipeline_artifacts::PipelineArtifact {
        let parse_json = |s: Option<String>| -> Option<serde_json::Value> {
            s.and_then(|j| serde_json::from_str(&j).ok())
        };
        let parse_json_vec = |s: Option<String>| -> Option<Vec<serde_json::Value>> {
            s.and_then(|j| serde_json::from_str(&j).ok())
        };

        crate::workflow_generation::pipeline_artifacts::PipelineArtifact {
            id: row.get(0).unwrap_or_default(),
            workflow_id: row.get(1).unwrap_or(None),
            task_run_id: row.get(2).unwrap_or(None),
            description: row.get(3).unwrap_or_default(),
            category: row.get(4).unwrap_or(None),
            created_at: row.get(5).unwrap_or_default(),
            investigation_duration_ms: row
                .get::<_, Option<i64>>(6)
                .unwrap_or(None)
                .map(|v| v as u64),
            investigation_enriched_description: row.get(7).unwrap_or(None),
            specification_duration_ms: row
                .get::<_, Option<i64>>(8)
                .unwrap_or(None)
                .map(|v| v as u64),
            specification_criteria: parse_json(row.get(9).unwrap_or(None)),
            specification_prompt: row.get(10).unwrap_or(None),
            builder_prompt: row.get(11).unwrap_or(None),
            verification_prompts: parse_json(row.get(12).unwrap_or(None)),
            hardener_prompt: row.get(13).unwrap_or(None),
            discovery_duration_ms: row
                .get::<_, Option<i64>>(14)
                .unwrap_or(None)
                .map(|v| v as u64),
            builder_duration_ms: row
                .get::<_, Option<i64>>(15)
                .unwrap_or(None)
                .map(|v| v as u64),
            autofix_duration_ms: row
                .get::<_, Option<i64>>(16)
                .unwrap_or(None)
                .map(|v| v as u64),
            verification_duration_ms: row
                .get::<_, Option<i64>>(17)
                .unwrap_or(None)
                .map(|v| v as u64),
            hardener_duration_ms: row
                .get::<_, Option<i64>>(18)
                .unwrap_or(None)
                .map(|v| v as u64),
            total_duration_ms: row
                .get::<_, Option<i64>>(19)
                .unwrap_or(None)
                .map(|v| v as u64),
            discovery_calls: parse_json(row.get(20).unwrap_or(None)),
            builder_raw_output: row.get(21).unwrap_or(None),
            builder_parsed_json: parse_json(row.get(22).unwrap_or(None)),
            autofix_diff: parse_json(row.get(23).unwrap_or(None)),
            verification_iterations: parse_json(row.get(24).unwrap_or(None)),
            fixer_snapshots: parse_json_vec(row.get(25).unwrap_or(None)),
            hardening_summary: parse_json(row.get(26).unwrap_or(None)),
            hardened_json: parse_json(row.get(27).unwrap_or(None)),
            final_json: parse_json(row.get(28).unwrap_or(None)),
            validation_errors: parse_json(row.get(29).unwrap_or(None)),
            success: row.get(30).unwrap_or(true),
            error_message: row.get(31).unwrap_or(None),
            model_used: row.get(32).unwrap_or(None),
            revision_duration_ms: row
                .get::<_, Option<i64>>(33)
                .unwrap_or(None)
                .map(|v| v as u64),
            quality_report: parse_json(row.get(34).unwrap_or(None)),
            revision_cycles: row
                .get::<_, Option<i32>>(35)
                .unwrap_or(None)
                .map(|v| v as u32),
            confidence_score: row
                .get::<_, Option<f64>>(36)
                .unwrap_or(None)
                .map(|v| v as f32),
            consistency_report: None, // Not stored in DB yet
            pipeline_depth: None, // Not stored in DB yet
            code_graph_stats: None, // Not stored in DB yet
        }
    }

    // ========================================================================
    // Generator Evaluation - Dashboard Metrics
    // ========================================================================

    /// Get aggregated dashboard metrics for generator evaluation.
    pub fn get_generation_dashboard_metrics(&self) -> Result<GeneratorDashboardMetrics, String> {
        let conn = self.get_conn()?;

        // Total generations and success rate from pipeline artifacts
        let (total_generations, successful_generations, avg_total_duration): (
            i64,
            i64,
            Option<f64>,
        ) = conn
            .query_row(
                r#"SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(total_duration_ms) as avg_duration
                FROM generation_pipeline_artifacts"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap_or((0, 0, None));

        // Average verification iterations
        let avg_verification_iterations: Option<f64> = conn
            .query_row(
                r#"SELECT AVG(json_array_length(verification_iterations))
                FROM generation_pipeline_artifacts
                WHERE verification_iterations IS NOT NULL AND verification_iterations != '[]'"#,
                [],
                |row| row.get(0),
            )
            .unwrap_or(None);

        // Verification first-pass rate (iterations with 0 issues on first pass)
        let first_pass_rate: Option<f64> = conn
            .query_row(
                r#"SELECT
                    CAST(SUM(CASE
                        WHEN json_extract(verification_iterations, '$[0].issues') = '[]'
                        THEN 1 ELSE 0
                    END) AS REAL) / NULLIF(COUNT(*), 0)
                FROM generation_pipeline_artifacts
                WHERE verification_iterations IS NOT NULL AND verification_iterations != '[]'"#,
                [],
                |row| row.get(0),
            )
            .unwrap_or(None);

        // Hardener conversion rate
        let (total_hardened, total_converted): (i64, i64) = conn
            .query_row(
                r#"SELECT
                    COUNT(*) as total,
                    SUM(COALESCE(json_extract(hardening_summary, '$.converted_count'), 0)) as converted
                FROM generation_pipeline_artifacts
                WHERE hardening_summary IS NOT NULL"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        // User feedback metrics from workflow_generation_feedback
        let (total_edits, total_deletes, total_ratings, avg_rating): (i64, i64, i64, Option<f64>) =
            conn.query_row(
                r#"SELECT
                    SUM(CASE WHEN feedback_type = 'edit' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN feedback_type = 'delete' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN feedback_type = 'rating' THEN 1 ELSE 0 END),
                    AVG(CASE WHEN feedback_type = 'rating' THEN rating ELSE NULL END)
                FROM workflow_generation_feedback"#,
                [],
                |row| {
                    Ok((
                        row.get(0).unwrap_or(0),
                        row.get(1).unwrap_or(0),
                        row.get(2).unwrap_or(0),
                        row.get(3)?,
                    ))
                },
            )
            .unwrap_or((0, 0, 0, None));

        Ok(GeneratorDashboardMetrics {
            total_generations,
            successful_generations,
            success_rate: if total_generations > 0 {
                successful_generations as f64 / total_generations as f64
            } else {
                0.0
            },
            avg_total_duration_ms: avg_total_duration,
            avg_verification_iterations,
            first_pass_rate,
            hardener_total_processed: total_hardened,
            hardener_total_converted: total_converted,
            total_edits,
            total_deletes,
            total_ratings,
            avg_rating,
        })
    }

    /// Get generation metrics over time (daily aggregates).
    pub fn get_generation_metrics_over_time(
        &self,
        days: u32,
    ) -> Result<Vec<GeneratorTimeSeriesPoint>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT
                    date(created_at) as day,
                    COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(total_duration_ms) as avg_duration,
                    AVG(json_array_length(verification_iterations)) as avg_iterations
                FROM generation_pipeline_artifacts
                WHERE created_at >= datetime('now', ?1)
                GROUP BY date(created_at)
                ORDER BY day ASC"#,
            )
            .map_err(|e| format!("Failed to prepare trends query: {}", e))?;

        let rows = stmt
            .query_map(params![format!("-{} days", days)], |row| {
                Ok(GeneratorTimeSeriesPoint {
                    date: row.get(0)?,
                    total_generations: row.get(1)?,
                    successful_generations: row.get(2)?,
                    avg_duration_ms: row.get(3)?,
                    avg_verification_iterations: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to query trends: {}", e))?;

        let mut points = Vec::new();
        for row in rows {
            points.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(points)
    }

    // ========================================================================
    // Generator Evaluation - Benchmarks
    // ========================================================================

    /// Save a new benchmark.
    pub fn save_benchmark(&self, benchmark: &GeneratorBenchmark) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generator_benchmarks
                (id, name, description, category, tags, expected_structure, created_at, updated_at, enabled)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                benchmark.id,
                benchmark.name,
                benchmark.description,
                benchmark.category,
                benchmark.tags.as_ref().map(|t| serde_json::to_string(t).unwrap_or_default()),
                serde_json::to_string(&benchmark.expected_structure).unwrap_or_default(),
                benchmark.created_at,
                benchmark.updated_at,
                benchmark.enabled,
            ],
        )
        .map_err(|e| format!("Failed to save benchmark: {}", e))?;
        Ok(())
    }

    /// Get a benchmark by ID.
    pub fn get_benchmark(&self, id: &str) -> Result<Option<GeneratorBenchmark>, String> {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, name, description, category, tags, expected_structure, created_at, updated_at, enabled
               FROM generator_benchmarks WHERE id = ?1"#,
            params![id],
            |row| Ok(Self::row_to_benchmark(row)),
        );
        match result {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get benchmark: {}", e)),
        }
    }

    /// List all benchmarks.
    pub fn list_benchmarks(&self) -> Result<Vec<GeneratorBenchmark>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, name, description, category, tags, expected_structure, created_at, updated_at, enabled
                   FROM generator_benchmarks ORDER BY created_at DESC"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map([], |row| Ok(Self::row_to_benchmark(row)))
            .map_err(|e| format!("Failed to list benchmarks: {}", e))?;

        let mut benchmarks = Vec::new();
        for row in rows {
            benchmarks.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(benchmarks)
    }

    /// Check if a benchmark with the given name already exists.
    pub fn benchmark_exists_by_name(&self, name: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generator_benchmarks WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check benchmark: {}", e))?;
        Ok(count > 0)
    }

    /// Update a benchmark.
    pub fn update_benchmark(&self, id: &str, update: &BenchmarkUpdate) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        if let Some(ref name) = update.name {
            conn.execute(
                "UPDATE generator_benchmarks SET name = ?1, updated_at = ?2 WHERE id = ?3",
                params![name, now, id],
            )
            .map_err(|e| format!("Failed to update name: {}", e))?;
        }
        if let Some(ref description) = update.description {
            conn.execute(
                "UPDATE generator_benchmarks SET description = ?1, updated_at = ?2 WHERE id = ?3",
                params![description, now, id],
            )
            .map_err(|e| format!("Failed to update description: {}", e))?;
        }
        if let Some(ref expected) = update.expected_structure {
            let json = serde_json::to_string(expected).unwrap_or_default();
            conn.execute(
                "UPDATE generator_benchmarks SET expected_structure = ?1, updated_at = ?2 WHERE id = ?3",
                params![json, now, id],
            )
            .map_err(|e| format!("Failed to update expected_structure: {}", e))?;
        }
        if let Some(enabled) = update.enabled {
            conn.execute(
                "UPDATE generator_benchmarks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled, now, id],
            )
            .map_err(|e| format!("Failed to update enabled: {}", e))?;
        }
        Ok(())
    }

    /// Delete a benchmark and its results.
    pub fn delete_benchmark(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "DELETE FROM generator_benchmark_results WHERE benchmark_id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete results: {}", e))?;
        conn.execute(
            "DELETE FROM generator_benchmarks WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete benchmark: {}", e))?;
        Ok(())
    }

    /// Save a benchmark result.
    pub fn save_benchmark_result(&self, result: &GeneratorBenchmarkResult) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generator_benchmark_results
                (id, benchmark_id, artifact_id, run_at, model_used,
                 structure_score, content_score, step_type_score, overall_score,
                 score_breakdown, generated_json, duration_ms, passed, notes)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
            params![
                result.id,
                result.benchmark_id,
                result.artifact_id,
                result.run_at,
                result.model_used,
                result.structure_score,
                result.content_score,
                result.step_type_score,
                result.overall_score,
                result.score_breakdown.as_ref().map(|v| v.to_string()),
                result.generated_json.as_ref().map(|v| v.to_string()),
                result.duration_ms.map(|v| v as i64),
                result.passed,
                result.notes,
            ],
        )
        .map_err(|e| format!("Failed to save benchmark result: {}", e))?;
        Ok(())
    }

    /// List results for a specific benchmark.
    pub fn list_benchmark_results(
        &self,
        benchmark_id: &str,
        limit: u32,
    ) -> Result<Vec<GeneratorBenchmarkResult>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, benchmark_id, artifact_id, run_at, model_used,
                          structure_score, content_score, step_type_score, overall_score,
                          score_breakdown, generated_json, duration_ms, passed, notes
                   FROM generator_benchmark_results
                   WHERE benchmark_id = ?1
                   ORDER BY run_at DESC
                   LIMIT ?2"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map(params![benchmark_id, limit], |row| {
                Ok(GeneratorBenchmarkResult {
                    id: row.get(0)?,
                    benchmark_id: row.get(1)?,
                    artifact_id: row.get(2)?,
                    run_at: row.get(3)?,
                    model_used: row.get(4)?,
                    structure_score: row.get(5)?,
                    content_score: row.get(6)?,
                    step_type_score: row.get(7)?,
                    overall_score: row.get(8)?,
                    score_breakdown: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    generated_json: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    duration_ms: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                    passed: row.get(12)?,
                    notes: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to list results: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    fn row_to_benchmark(row: &rusqlite::Row) -> GeneratorBenchmark {
        let tags_json: Option<String> = row.get(4).unwrap_or(None);
        let expected_json: String = row.get(5).unwrap_or_default();
        GeneratorBenchmark {
            id: row.get(0).unwrap_or_default(),
            name: row.get(1).unwrap_or_default(),
            description: row.get(2).unwrap_or_default(),
            category: row.get(3).unwrap_or(None),
            tags: tags_json.and_then(|j| serde_json::from_str(&j).ok()),
            expected_structure: serde_json::from_str(&expected_json).unwrap_or_default(),
            created_at: row.get(6).unwrap_or_default(),
            updated_at: row.get(7).unwrap_or_default(),
            enabled: row.get(8).unwrap_or(true),
        }
    }

    // ========================================================================
    // Generator Evaluation - Edit Analysis
    // ========================================================================

    /// Get aggregated edit analysis from workflow_generation_feedback.
    pub fn get_edit_analysis(&self) -> Result<EditAnalysis, String> {
        let conn = self.get_conn()?;

        // Most commonly edited fields
        let mut stmt = conn
            .prepare(
                r#"SELECT edited_field, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'edit' AND edited_field IS NOT NULL
                   GROUP BY edited_field
                   ORDER BY cnt DESC
                   LIMIT 20"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let edited_fields: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Feedback type distribution
        let mut stmt = conn
            .prepare(
                r#"SELECT feedback_type, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   GROUP BY feedback_type
                   ORDER BY cnt DESC"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let type_distribution: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Rating distribution
        let mut stmt = conn
            .prepare(
                r#"SELECT rating, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'rating' AND rating IS NOT NULL
                   GROUP BY rating
                   ORDER BY rating"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rating_distribution: Vec<(i32, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Recent edits
        let mut stmt = conn
            .prepare(
                r#"SELECT f.id, f.workflow_id, f.feedback_type, f.edited_field,
                          f.old_value, f.new_value, f.created_at,
                          w.name as workflow_name
                   FROM workflow_generation_feedback f
                   LEFT JOIN unified_workflows w ON f.workflow_id = w.id
                   ORDER BY f.created_at DESC
                   LIMIT 50"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let recent_feedback: Vec<RecentFeedback> = stmt
            .query_map([], |row| {
                Ok(RecentFeedback {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    feedback_type: row.get(2)?,
                    edited_field: row.get(3)?,
                    old_value: row.get(4)?,
                    new_value: row.get(5)?,
                    created_at: row.get(6)?,
                    workflow_name: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(EditAnalysis {
            edited_fields,
            type_distribution,
            rating_distribution,
            recent_feedback,
        })
    }

    /// Save a feedback entry to workflow_generation_feedback.
    pub fn save_generator_feedback(
        &self,
        id: &str,
        workflow_id: &str,
        workflow_name: Option<&str>,
        feedback_type: &str,
        edited_field: Option<&str>,
        old_value: Option<&str>,
        new_value: Option<&str>,
        rating: Option<i32>,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO workflow_generation_feedback
                (id, workflow_id, feedback_type, edited_field, old_value, new_value, rating, workflow_description, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                id,
                workflow_id,
                feedback_type,
                edited_field,
                old_value,
                new_value,
                rating,
                workflow_name,
                created_at,
            ],
        )
        .map_err(|e| format!("Failed to save generator feedback: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Generator Evaluation - Example Library
    // ========================================================================

    /// List workflows that have example_status set.
    pub fn list_example_workflows(&self) -> Result<Vec<ExampleWorkflowSummary>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, name, description, category, example_status, created_at
                   FROM unified_workflows
                   WHERE example_status IS NOT NULL AND example_status != ''
                   ORDER BY example_status, name"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ExampleWorkflowSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    category: row.get(3)?,
                    example_status: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Failed to query: {}", e))?;

        let mut examples = Vec::new();
        for row in rows {
            examples.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(examples)
    }

    /// Update a workflow's example_status.
    pub fn update_example_status(
        &self,
        workflow_id: &str,
        status: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET example_status = ?1 WHERE id = ?2",
            params![status, workflow_id],
        )
        .map_err(|e| format!("Failed to update example_status: {}", e))?;
        Ok(())
    }
}
