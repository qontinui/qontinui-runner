//! PostgreSQL adaptive learning operations.
//!
//! Playbook entries, curated examples, template performance, GEPA runs,
//! template lifecycle events, and learning trends.

use super::PgDb;

impl PgDb {
    // =========================================================================
    // Playbook Entries
    // =========================================================================

    pub async fn insert_playbook_entry(
        &self,
        id: &str,
        lesson: &str,
        category: &str,
        domain: Option<&str>,
        severity: &str,
        source_run_id: &str,
        source_step_id: Option<&str>,
        positive: bool,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let positive_int: i32 = if positive { 1 } else { 0 };
        conn.execute(
            "INSERT INTO playbook_entries (id, lesson, category, domain, severity, source_run_id, source_step_id, positive)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO NOTHING",
            &[&id, &lesson, &category, &domain, &severity, &source_run_id, &source_step_id, &positive_int],
        ).await.map_err(|e| format!("Insert playbook entry failed: {}", e))?;
        Ok(())
    }

    /// List playbook entries with optional domain/status filters.
    /// Used by the Tauri command `get_playbook_entries`.
    pub async fn list_playbook_entries(
        &self,
        domain: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build query with dynamic parameters
        let mut query = String::from(
            "SELECT id, lesson, category, domain, severity, positive, times_applied, times_helped, status, created_at
             FROM playbook_entries WHERE 1=1",
        );
        let mut param_values: Vec<String> = Vec::new();
        let mut idx = 0usize;

        if let Some(d) = domain {
            idx += 1;
            query.push_str(&format!(" AND domain = ${}", idx));
            param_values.push(d.to_string());
        }
        if let Some(s) = status {
            idx += 1;
            query.push_str(&format!(" AND status = ${}", idx));
            param_values.push(s.to_string());
        }

        query.push_str(
            " ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'important' THEN 1 ELSE 2 END,
              times_helped DESC",
        );
        idx += 1;
        query.push_str(&format!(" LIMIT ${}", idx));

        // Build param refs
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        for v in &param_values {
            params.push(v);
        }
        params.push(&limit);

        let rows = conn
            .query(&query, &params)
            .await
            .map_err(|e| format!("Query playbook entries failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let times_applied: i32 = row.get(6);
                let times_helped: i32 = row.get(7);
                let helpfulness = if times_applied > 0 {
                    times_helped as f64 / times_applied as f64
                } else {
                    0.0
                };
                let positive_int: i32 = row.get(5);
                let created_at: chrono::DateTime<chrono::Utc> = row.get(9);

                serde_json::json!({
                    "id": row.get::<_, String>(0),
                    "lesson": row.get::<_, String>(1),
                    "category": row.get::<_, String>(2),
                    "domain": row.get::<_, Option<String>>(3),
                    "severity": row.get::<_, String>(4),
                    "positive": positive_int != 0,
                    "times_applied": times_applied as i64,
                    "times_helped": times_helped as i64,
                    "helpfulness_ratio": helpfulness,
                    "status": row.get::<_, String>(8),
                    "created_at": created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(results)
    }

    /// Keep the old method name for any existing callers.
    pub async fn get_playbook_entries(
        &self,
        domain: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        self.list_playbook_entries(domain, status, limit).await
    }

    pub async fn update_playbook_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE playbook_entries SET status = $1, updated_at = NOW() WHERE id = $2",
            &[&status, &id],
        )
        .await
        .map_err(|e| format!("Update playbook status failed: {}", e))?;
        Ok(())
    }

    pub async fn delete_playbook_entry(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM playbook_entries WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("Delete playbook entry failed: {}", e))?;
        Ok(())
    }

    /// Get full detail for a single playbook entry plus its source run context.
    pub async fn get_playbook_entry_detail(&self, id: &str) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "SELECT id, lesson, category, domain, severity, source_run_id, source_step_id,
                        positive, times_applied, times_helped, status, created_at, updated_at
                 FROM playbook_entries WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("Playbook entry not found: {}", e))?;

        let times_applied: i32 = row.get(8);
        let times_helped: i32 = row.get(9);
        let helpfulness = if times_applied > 0 {
            times_helped as f64 / times_applied as f64
        } else {
            0.0
        };
        let positive_int: i32 = row.get(7);
        let created_at: chrono::DateTime<chrono::Utc> = row.get(11);
        let updated_at: chrono::DateTime<chrono::Utc> = row.get(12);
        let source_run_id: String = row.get(5);

        let entry = serde_json::json!({
            "id": row.get::<_, String>(0),
            "lesson": row.get::<_, String>(1),
            "category": row.get::<_, String>(2),
            "domain": row.get::<_, Option<String>>(3),
            "severity": row.get::<_, String>(4),
            "source_run_id": &source_run_id,
            "source_step_id": row.get::<_, Option<String>>(6),
            "positive": positive_int != 0,
            "times_applied": times_applied as i64,
            "times_helped": times_helped as i64,
            "helpfulness_ratio": helpfulness,
            "status": row.get::<_, String>(10),
            "created_at": created_at.to_rfc3339(),
            "updated_at": updated_at.to_rfc3339(),
        });

        // Try to get source run context from learning_outcomes
        let run_context = conn
            .query_opt(
                "SELECT id, status, duration_secs, iterations, strategy, workflow_architecture, created_at
                 FROM learning_outcomes WHERE task_id = $1",
                &[&source_run_id],
            )
            .await
            .ok()
            .flatten()
            .map(|r| {
                let lo_created: chrono::DateTime<chrono::Utc> = r.get(6);
                serde_json::json!({
                    "run_id": r.get::<_, String>(0),
                    "status": r.get::<_, String>(1),
                    "duration_secs": r.get::<_, Option<f64>>(2),
                    "iterations": r.get::<_, Option<i32>>(3),
                    "strategy": r.get::<_, Option<String>>(4),
                    "architecture": r.get::<_, Option<String>>(5),
                    "created_at": lo_created.to_rfc3339(),
                })
            });

        Ok(serde_json::json!({
            "entry": entry,
            "source_run": run_context,
        }))
    }

    pub async fn increment_playbook_applied(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE playbook_entries SET times_applied = times_applied + 1, updated_at = NOW() WHERE id = $1",
            &[&id],
        ).await.map_err(|e| format!("Increment applied failed: {}", e))?;
        Ok(())
    }

    pub async fn increment_playbook_helped(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE playbook_entries SET times_helped = times_helped + 1, updated_at = NOW() WHERE id = $1",
            &[&id],
        ).await.map_err(|e| format!("Increment helped failed: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Curated Examples
    // =========================================================================

    pub async fn insert_curated_example(
        &self,
        id: &str,
        domain: &str,
        criterion_description: &str,
        steps_json: &str,
        quality_score: f64,
        execution_verified: bool,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let verified_int: i32 = if execution_verified { 1 } else { 0 };
        conn.execute(
            "INSERT INTO curated_examples (id, domain, criterion_description, steps_json, quality_score, execution_verified)
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
            &[&id, &domain, &criterion_description, &steps_json, &quality_score, &verified_int],
        ).await.map_err(|e| format!("Insert curated example failed: {}", e))?;
        Ok(())
    }

    pub async fn get_curated_examples_by_domain(
        &self,
        domain: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT id, domain, criterion_description, steps_json, quality_score, execution_verified, times_used, created_at
             FROM curated_examples WHERE domain = $1 ORDER BY quality_score DESC LIMIT $2",
            &[&domain, &limit],
        ).await.map_err(|e| format!("Query curated examples failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let verified_int: i32 = row.get(5);
                let created_at: chrono::DateTime<chrono::Utc> = row.get(7);
                serde_json::json!({
                    "id": row.get::<_, String>(0),
                    "domain": row.get::<_, String>(1),
                    "criterion_description": row.get::<_, String>(2),
                    "steps_json": row.get::<_, String>(3),
                    "quality_score": row.get::<_, f64>(4),
                    "execution_verified": verified_int != 0,
                    "times_used": row.get::<_, i32>(6) as i64,
                    "created_at": created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(results)
    }

    pub async fn delete_curated_example(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM curated_examples WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("Delete curated example failed: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Template Performance
    // =========================================================================

    pub async fn upsert_template_performance(
        &self,
        template_id: &str,
        template_name: &str,
        passed: bool,
        quality_score: f64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (success_inc, failure_inc) = if passed { (1i32, 0i32) } else { (0i32, 1i32) };
        conn.execute(
            "INSERT INTO template_performance (template_id, template_name, success_count, failure_count, total_quality_score, last_used_at)
             VALUES ($1, $2, $3, $4, $5, NOW())
             ON CONFLICT (template_id) DO UPDATE SET
                success_count = template_performance.success_count + $3,
                failure_count = template_performance.failure_count + $4,
                total_quality_score = template_performance.total_quality_score + $5,
                last_used_at = NOW(),
                updated_at = NOW()",
            &[&template_id, &template_name, &success_inc, &failure_inc, &quality_score],
        ).await.map_err(|e| format!("Upsert template performance failed: {}", e))?;
        Ok(())
    }

    pub async fn get_all_template_performance(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT template_id, template_name, source, success_count, failure_count,
                    total_quality_score, last_used_at::TEXT
             FROM template_performance
             ORDER BY (success_count + failure_count) DESC",
                &[],
            )
            .await
            .map_err(|e| format!("Query template performance failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let success: i32 = row.get(3);
                let failure: i32 = row.get(4);
                let total_quality: f64 = row.get(5);
                let total = success + failure;
                let confidence = if total > 0 {
                    success as f64 / total as f64
                } else {
                    0.0
                };
                let avg_quality = if total > 0 {
                    total_quality / total as f64
                } else {
                    0.0
                };

                serde_json::json!({
                    "template_id": row.get::<_, String>(0),
                    "template_name": row.get::<_, String>(1),
                    "source": row.get::<_, String>(2),
                    "success_count": success as i64,
                    "failure_count": failure as i64,
                    "confidence": confidence,
                    "avg_quality": avg_quality,
                    "last_used_at": row.get::<_, Option<String>>(6),
                })
            })
            .collect();

        Ok(results)
    }

    pub async fn update_template_source(
        &self,
        template_id: &str,
        source: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE template_performance SET source = $1, updated_at = NOW() WHERE template_id = $2",
            &[&source, &template_id],
        ).await.map_err(|e| format!("Update template source failed: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Template Lifecycle Events
    // =========================================================================

    pub async fn get_template_lifecycle_events(
        &self,
        template_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT id, template_id, action, old_source, new_source, confidence_at_transition, created_at
             FROM template_lifecycle_events
             WHERE template_id = $1
             ORDER BY created_at DESC",
            &[&template_id],
        ).await.map_err(|e| format!("Query template lifecycle events failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let created_at: chrono::DateTime<chrono::Utc> = row.get(6);
                serde_json::json!({
                    "id": row.get::<_, String>(0),
                    "template_id": row.get::<_, String>(1),
                    "action": row.get::<_, String>(2),
                    "old_source": row.get::<_, String>(3),
                    "new_source": row.get::<_, String>(4),
                    "confidence_at_transition": row.get::<_, f64>(5),
                    "created_at": created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(results)
    }

    // =========================================================================
    // GEPA Optimization Runs
    // =========================================================================

    pub async fn insert_gepa_run(
        &self,
        id: &str,
        domain: &str,
        old_instructions: &str,
        new_instructions: Option<&str>,
        old_score: Option<f64>,
        new_score: Option<f64>,
        improvement: Option<f64>,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO gepa_optimization_runs (id, domain, old_instructions, new_instructions, old_score, new_score, improvement, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[&id, &domain, &old_instructions, &new_instructions, &old_score, &new_score, &improvement, &status],
        ).await.map_err(|e| format!("Insert GEPA run failed: {}", e))?;
        Ok(())
    }

    pub async fn get_recent_gepa_runs(&self, limit: i64) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, domain, old_score, new_score, improvement, status, created_at
             FROM gepa_optimization_runs ORDER BY created_at DESC LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("Query GEPA runs failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let created_at: chrono::DateTime<chrono::Utc> = row.get(6);
                serde_json::json!({
                    "id": row.get::<_, String>(0),
                    "domain": row.get::<_, String>(1),
                    "old_score": row.get::<_, Option<f64>>(2),
                    "new_score": row.get::<_, Option<f64>>(3),
                    "improvement": row.get::<_, Option<f64>>(4),
                    "status": row.get::<_, String>(5),
                    "created_at": created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(results)
    }

    /// Get full detail for a single GEPA run (including instructions).
    pub async fn get_gepa_run_detail(&self, id: &str) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT id, domain, old_instructions, new_instructions, old_score, new_score,
                        improvement, status, created_at
                 FROM gepa_optimization_runs WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("GEPA run not found: {}", e))?;

        let created_at: chrono::DateTime<chrono::Utc> = row.get(8);

        Ok(serde_json::json!({
            "id": row.get::<_, String>(0),
            "domain": row.get::<_, String>(1),
            "old_instructions": row.get::<_, String>(2),
            "new_instructions": row.get::<_, Option<String>>(3),
            "old_score": row.get::<_, Option<f64>>(4),
            "new_score": row.get::<_, Option<f64>>(5),
            "improvement": row.get::<_, Option<f64>>(6),
            "status": row.get::<_, String>(7),
            "created_at": created_at.to_rfc3339(),
        }))
    }

    // =========================================================================
    // Adaptive Learning Stats (aggregate)
    // =========================================================================

    pub async fn get_adaptive_learning_stats(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let playbook_total: i64 = conn
            .query_one("SELECT COUNT(*) FROM playbook_entries", &[])
            .await
            .map_err(|e| format!("Count playbook: {}", e))?
            .get(0);

        let active: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM playbook_entries WHERE status = 'active'",
                &[],
            )
            .await
            .map_err(|e| format!("Count active: {}", e))?
            .get(0);

        let staged: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM playbook_entries WHERE status = 'staged'",
                &[],
            )
            .await
            .map_err(|e| format!("Count staged: {}", e))?
            .get(0);

        let retired: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM playbook_entries WHERE status = 'retired'",
                &[],
            )
            .await
            .map_err(|e| format!("Count retired: {}", e))?
            .get(0);

        let examples: i64 = conn
            .query_one("SELECT COUNT(*) FROM curated_examples", &[])
            .await
            .map_err(|e| format!("Count examples: {}", e))?
            .get(0);

        let templates: i64 = conn
            .query_one("SELECT COUNT(*) FROM template_performance", &[])
            .await
            .map_err(|e| format!("Count templates: {}", e))?
            .get(0);

        let gepa: i64 = conn
            .query_one("SELECT COUNT(*) FROM gepa_optimization_runs", &[])
            .await
            .map_err(|e| format!("Count GEPA: {}", e))?
            .get(0);

        let avg_helpfulness: f64 = conn
            .query_one(
                "SELECT COALESCE(AVG(times_helped::double precision / NULLIF(times_applied, 0)), 0.0)
                 FROM playbook_entries WHERE times_applied > 0",
                &[],
            )
            .await
            .map_err(|e| format!("Avg helpfulness: {}", e))?
            .get(0);

        Ok(serde_json::json!({
            "playbook_entries": playbook_total,
            "active_lessons": active,
            "staged_lessons": staged,
            "retired_lessons": retired,
            "curated_examples": examples,
            "templates_tracked": templates,
            "gepa_runs": gepa,
            "avg_lesson_helpfulness": avg_helpfulness,
        }))
    }

    /// Keep the old method for any existing callers.
    pub async fn get_learning_stats(&self) -> Result<serde_json::Value, String> {
        self.get_adaptive_learning_stats().await
    }

    // =========================================================================
    // Learning Trends
    // =========================================================================

    pub async fn get_learning_trends(&self, days: i64) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let interval = format!("{} days", days);

        // Playbook entries per day
        let lesson_rows = conn
            .query(
                "SELECT created_at::date::text AS day, COUNT(*) AS count, severity
             FROM playbook_entries
             WHERE created_at >= NOW() - $1::interval
             GROUP BY day, severity
             ORDER BY day",
                &[&interval],
            )
            .await
            .map_err(|e| format!("Lesson trends: {}", e))?;

        let lesson_trends: Vec<serde_json::Value> = lesson_rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "day": row.get::<_, String>(0),
                    "count": row.get::<_, i64>(1),
                    "severity": row.get::<_, String>(2),
                })
            })
            .collect();

        // Curated examples per day
        let example_rows = conn
            .query(
                "SELECT created_at::date::text AS day, COUNT(*) AS count, domain
             FROM curated_examples
             WHERE created_at >= NOW() - $1::interval
             GROUP BY day, domain
             ORDER BY day",
                &[&interval],
            )
            .await
            .map_err(|e| format!("Example trends: {}", e))?;

        let example_trends: Vec<serde_json::Value> = example_rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "day": row.get::<_, String>(0),
                    "count": row.get::<_, i64>(1),
                    "domain": row.get::<_, String>(2),
                })
            })
            .collect();

        // Domain distribution
        let domain_rows = conn
            .query(
                "SELECT COALESCE(domain, 'general') AS domain, COUNT(*) AS count
             FROM playbook_entries WHERE status != 'retired'
             GROUP BY domain ORDER BY count DESC",
                &[],
            )
            .await
            .map_err(|e| format!("Domain distribution: {}", e))?;

        let domain_dist: Vec<serde_json::Value> = domain_rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "domain": row.get::<_, String>(0),
                    "count": row.get::<_, i64>(1),
                })
            })
            .collect();

        // Helpfulness over time
        let helpfulness_rows = conn.query(
            "SELECT updated_at::date::text AS day,
                    AVG(times_helped::double precision / NULLIF(times_applied, 0)) AS avg_helpfulness,
                    SUM(times_applied) AS total_applied
             FROM playbook_entries
             WHERE times_applied > 0 AND updated_at >= NOW() - $1::interval
             GROUP BY day ORDER BY day",
            &[&interval],
        ).await.map_err(|e| format!("Helpfulness trends: {}", e))?;

        let helpfulness_trends: Vec<serde_json::Value> = helpfulness_rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "day": row.get::<_, String>(0),
                    "avg_helpfulness": row.get::<_, Option<f64>>(1),
                    "total_applied": row.get::<_, i64>(2),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "lesson_trends": lesson_trends,
            "example_trends": example_trends,
            "domain_distribution": domain_dist,
            "helpfulness_trends": helpfulness_trends,
        }))
    }
}
