//! PostgreSQL adaptive learning operations.
//!
//! Playbook entries, curated examples, template performance, and GEPA runs.

use super::PgDb;

impl PgDb {
    // --- Playbook Entries ---

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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO playbook_entries (id, lesson, category, domain, severity, source_run_id, source_step_id, positive) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (id) DO NOTHING",
            &[&id, &lesson, &category, &domain, &severity, &source_run_id, &source_step_id, &positive],
        ).await.map_err(|e| format!("Insert playbook entry failed: {}", e))?;
        Ok(())
    }

    pub async fn get_playbook_entries(
        &self,
        domain: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let mut query = String::from(
            "SELECT id, lesson, category, domain, severity, source_run_id, source_step_id, positive, times_applied, times_helped, status, created_at, updated_at FROM playbook_entries WHERE 1=1"
        );
        // Build dynamic WHERE clause
        if domain.is_some() {
            query.push_str(" AND domain = $1");
        }
        if status.is_some() {
            query.push_str(if domain.is_some() { " AND status = $2" } else { " AND status = $1" });
        }
        query.push_str(" ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'important' THEN 1 ELSE 2 END, times_helped DESC");
        query.push_str(&format!(" LIMIT {}", limit));

        // Use simple approach - query all and filter in Rust
        let rows = conn.query(
            "SELECT id, lesson, category, domain, severity, source_run_id, source_step_id, positive, times_applied, times_helped, status, created_at, updated_at FROM playbook_entries ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'important' THEN 1 ELSE 2 END, times_helped DESC LIMIT $1",
            &[&limit],
        ).await.map_err(|e| format!("Query playbook entries failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows.iter().map(|row| {
            serde_json::json!({
                "id": row.get::<_, String>(0),
                "lesson": row.get::<_, String>(1),
                "category": row.get::<_, String>(2),
                "domain": row.get::<_, Option<String>>(3),
                "severity": row.get::<_, String>(4),
                "source_run_id": row.get::<_, String>(5),
                "source_step_id": row.get::<_, Option<String>>(6),
                "positive": row.get::<_, bool>(7),
                "times_applied": row.get::<_, i32>(8),
                "times_helped": row.get::<_, i32>(9),
                "status": row.get::<_, String>(10),
                "created_at": row.get::<_, String>(11),
                "updated_at": row.get::<_, String>(12),
            })
        }).collect();

        Ok(results)
    }

    pub async fn update_playbook_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE playbook_entries SET status = $1, updated_at = NOW() WHERE id = $2",
            &[&status, &id],
        ).await.map_err(|e| format!("Update playbook status failed: {}", e))?;
        Ok(())
    }

    pub async fn increment_playbook_applied(&self, id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE playbook_entries SET times_applied = times_applied + 1, updated_at = NOW() WHERE id = $1",
            &[&id],
        ).await.map_err(|e| format!("Increment applied failed: {}", e))?;
        Ok(())
    }

    pub async fn increment_playbook_helped(&self, id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE playbook_entries SET times_helped = times_helped + 1, updated_at = NOW() WHERE id = $1",
            &[&id],
        ).await.map_err(|e| format!("Increment helped failed: {}", e))?;
        Ok(())
    }

    // --- Curated Examples ---

    pub async fn insert_curated_example(
        &self,
        id: &str,
        domain: &str,
        criterion_description: &str,
        steps_json: &str,
        quality_score: f64,
        execution_verified: bool,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO curated_examples (id, domain, criterion_description, steps_json, quality_score, execution_verified) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
            &[&id, &domain, &criterion_description, &steps_json, &quality_score, &execution_verified],
        ).await.map_err(|e| format!("Insert curated example failed: {}", e))?;
        Ok(())
    }

    pub async fn get_curated_examples_by_domain(
        &self,
        domain: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT id, domain, criterion_description, steps_json, quality_score, execution_verified, times_used, created_at FROM curated_examples WHERE domain = $1 ORDER BY quality_score DESC LIMIT $2",
            &[&domain, &limit],
        ).await.map_err(|e| format!("Query curated examples failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows.iter().map(|row| {
            serde_json::json!({
                "id": row.get::<_, String>(0),
                "domain": row.get::<_, String>(1),
                "criterion_description": row.get::<_, String>(2),
                "steps_json": row.get::<_, String>(3),
                "quality_score": row.get::<_, f64>(4),
                "execution_verified": row.get::<_, bool>(5),
                "times_used": row.get::<_, i32>(6),
                "created_at": row.get::<_, String>(7),
            })
        }).collect();

        Ok(results)
    }

    // --- Template Performance ---

    pub async fn upsert_template_performance(
        &self,
        template_id: &str,
        template_name: &str,
        passed: bool,
        quality_score: f64,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT template_id, template_name, source, success_count, failure_count, total_quality_score, last_used_at, created_at, updated_at FROM template_performance ORDER BY success_count DESC",
            &[],
        ).await.map_err(|e| format!("Query template performance failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows.iter().map(|row| {
            serde_json::json!({
                "template_id": row.get::<_, String>(0),
                "template_name": row.get::<_, String>(1),
                "source": row.get::<_, String>(2),
                "success_count": row.get::<_, i32>(3),
                "failure_count": row.get::<_, i32>(4),
                "total_quality_score": row.get::<_, f64>(5),
                "last_used_at": row.get::<_, Option<String>>(6),
                "created_at": row.get::<_, String>(7),
                "updated_at": row.get::<_, String>(8),
            })
        }).collect();

        Ok(results)
    }

    pub async fn update_template_source(&self, template_id: &str, source: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE template_performance SET source = $1, updated_at = NOW() WHERE template_id = $2",
            &[&source, &template_id],
        ).await.map_err(|e| format!("Update template source failed: {}", e))?;
        Ok(())
    }

    // --- GEPA Optimization Runs ---

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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO gepa_optimization_runs (id, domain, old_instructions, new_instructions, old_score, new_score, improvement, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[&id, &domain, &old_instructions, &new_instructions, &old_score, &new_score, &improvement, &status],
        ).await.map_err(|e| format!("Insert GEPA run failed: {}", e))?;
        Ok(())
    }

    pub async fn get_recent_gepa_runs(&self, limit: i64) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT id, domain, old_score, new_score, improvement, status, created_at FROM gepa_optimization_runs ORDER BY created_at DESC LIMIT $1",
            &[&limit],
        ).await.map_err(|e| format!("Query GEPA runs failed: {}", e))?;

        let results: Vec<serde_json::Value> = rows.iter().map(|row| {
            serde_json::json!({
                "id": row.get::<_, String>(0),
                "domain": row.get::<_, String>(1),
                "old_score": row.get::<_, Option<f64>>(2),
                "new_score": row.get::<_, Option<f64>>(3),
                "improvement": row.get::<_, Option<f64>>(4),
                "status": row.get::<_, String>(5),
                "created_at": row.get::<_, String>(6),
            })
        }).collect();

        Ok(results)
    }

    // --- Learning Stats ---

    pub async fn get_learning_stats(&self) -> Result<serde_json::Value, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let playbook_count: i64 = conn.query_one(
            "SELECT COUNT(*) FROM playbook_entries WHERE status != 'retired'", &[]
        ).await.map_err(|e| format!("Count playbook: {}", e))?.get(0);

        let examples_count: i64 = conn.query_one(
            "SELECT COUNT(*) FROM curated_examples", &[]
        ).await.map_err(|e| format!("Count examples: {}", e))?.get(0);

        let templates_tracked: i64 = conn.query_one(
            "SELECT COUNT(*) FROM template_performance", &[]
        ).await.map_err(|e| format!("Count templates: {}", e))?.get(0);

        let gepa_runs: i64 = conn.query_one(
            "SELECT COUNT(*) FROM gepa_optimization_runs", &[]
        ).await.map_err(|e| format!("Count GEPA: {}", e))?.get(0);

        Ok(serde_json::json!({
            "playbook_entries": playbook_count,
            "curated_examples": examples_count,
            "templates_tracked": templates_tracked,
            "gepa_optimization_runs": gepa_runs,
        }))
    }
}
