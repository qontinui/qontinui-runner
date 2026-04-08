//! PostgreSQL task_knowledge operations for the knowledge acquisition flywheel.
//!
//! Uses raw tokio-postgres queries. Switch to Clorinde-generated code once
//! `clorinde fresh` is run against the task_knowledge table.

use super::PgDb;
use crate::database::embeddings::{blob_to_vector, vector_to_blob};
use crate::database::hybrid_search::{HybridSearchConfig, KnowledgeResult, SearchResult};

impl PgDb {
    /// Insert a new task_knowledge entry. Returns the ID.
    pub async fn create_task_knowledge(
        &self,
        id: &str,
        task_run_id: &str,
        category: &str,
        agent_type: &str,
        iteration: i32,
        content: &str,
        evidence: Option<&str>,
        confidence: &str,
        related_files: &str,
    ) -> Result<String, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        conn.execute(
            r#"INSERT INTO task_knowledge
               (id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
            &[
                &id,
                &task_run_id,
                &category,
                &agent_type,
                &iteration,
                &content,
                &evidence,
                &confidence,
                &related_files,
            ],
        )
        .await
        .map_err(|e| format!("PG create_task_knowledge: {e}"))?;

        Ok(id.to_string())
    }

    /// Mark a task_knowledge entry as resolved.
    pub async fn resolve_task_knowledge(
        &self,
        knowledge_id: &str,
        notes: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        conn.execute(
            "UPDATE task_knowledge \
             SET is_resolved = TRUE, resolution_notes = $1, resolved_at = NOW() \
             WHERE id = $2",
            &[&notes, &knowledge_id],
        )
        .await
        .map_err(|e| format!("PG resolve_task_knowledge: {e}"))?;

        Ok(())
    }

    /// Update the project_path field on a task_knowledge entry.
    pub async fn update_task_knowledge_project_path(
        &self,
        knowledge_id: &str,
        project_path: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        conn.execute(
            "UPDATE task_knowledge SET project_path = $1 WHERE id = $2",
            &[&project_path, &knowledge_id],
        )
        .await
        .map_err(|e| format!("PG update_task_knowledge_project_path: {e}"))?;

        Ok(())
    }

    /// Store an embedding vector for a task_knowledge entry.
    pub async fn store_knowledge_embedding(
        &self,
        knowledge_id: &str,
        embedding: &[f32],
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        let blob = vector_to_blob(embedding);

        conn.execute(
            "UPDATE task_knowledge SET content_embedding = $1 WHERE id = $2",
            &[&blob, &knowledge_id],
        )
        .await
        .map_err(|e| format!("PG store_knowledge_embedding: {e}"))?;

        Ok(())
    }

    /// Best-effort: compute an embedding for the given knowledge content and
    /// persist it via `store_knowledge_embedding`.
    ///
    /// This is the write-side counterpart to `hybrid_search_knowledge`. Every
    /// live `create_task_knowledge` call site should invoke this immediately
    /// after a successful insert so that the row becomes searchable via the
    /// Tier 0 vector path.
    ///
    /// **Best-effort contract**: embedding failures (service down, network
    /// hiccup, storage error) MUST NOT fail the primary knowledge write.
    /// They are logged at warn level and this method returns `Ok(())`. Rows
    /// that fail to embed simply remain unsearchable via the vector path
    /// until the next time the content is re-embedded.
    pub async fn embed_knowledge_content(
        &self,
        knowledge_id: &str,
        content: &str,
    ) -> Result<(), String> {
        let client = crate::database::embedding_client::EmbeddingClient::new();
        let embedding = match client.compute_text_embedding(content).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    "embed_knowledge_content: compute failed for {}: {} (row saved without embedding)",
                    knowledge_id,
                    e
                );
                return Ok(());
            }
        };
        if let Err(e) = self.store_knowledge_embedding(knowledge_id, &embedding).await {
            tracing::warn!(
                "embed_knowledge_content: store failed for {}: {} (row saved without embedding)",
                knowledge_id,
                e
            );
        }
        Ok(())
    }

    /// Hybrid search task_knowledge: fetch candidates with embeddings, rank in-memory.
    ///
    /// 1. Fetch recent entries with embeddings (optionally filtered by category)
    /// 2. Compute cosine similarity in-memory
    /// 3. Return ranked results above min_similarity threshold
    pub async fn hybrid_search_knowledge(
        &self,
        query_embedding: &[f32],
        category: Option<&str>,
        config: &HybridSearchConfig,
    ) -> Result<Vec<SearchResult<KnowledgeResult>>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        let candidate_limit = (config.limit * 3) as i64;

        let rows = if let Some(cat) = category {
            conn.query(
                r#"SELECT id, task_run_id, category, content, confidence,
                          is_resolved, to_char(created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'), content_embedding
                   FROM task_knowledge
                   WHERE content_embedding IS NOT NULL
                     AND archived_at IS NULL
                     AND category = $1
                   ORDER BY created_at DESC
                   LIMIT $2"#,
                &[&cat, &candidate_limit],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, task_run_id, category, content, confidence,
                          is_resolved, to_char(created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'), content_embedding
                   FROM task_knowledge
                   WHERE content_embedding IS NOT NULL
                     AND archived_at IS NULL
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&candidate_limit],
            )
            .await
        }
        .map_err(|e| format!("PG hybrid_search_knowledge: {e}"))?;

        let candidates: Vec<(KnowledgeResult, Option<Vec<f32>>)> = rows
            .iter()
            .map(|row| {
                let embedding_blob: Option<Vec<u8>> = row.get(7);
                (
                    KnowledgeResult {
                        id: row.get(0),
                        task_run_id: row.get(1),
                        category: row.get(2),
                        content: row.get(3),
                        confidence: row.get(4),
                        is_resolved: row.get(5),
                        created_at: row.get::<_, String>(6),
                    },
                    embedding_blob.and_then(|b| blob_to_vector(&b)),
                )
            })
            .collect();

        Ok(crate::database::hybrid_search::rank_results(
            candidates,
            query_embedding,
            config,
        ))
    }

    /// List task knowledge entries for a specific task run.
    ///
    /// `include_archived` controls whether rows whose `archived_at` is set
    /// (i.e. compressed into a summary) are included. Pass `false` from any
    /// non-admin reader.
    pub async fn list_task_knowledge(
        &self,
        task_run_id: &str,
        category: Option<&str>,
        unresolved_only: bool,
        include_archived: bool,
    ) -> Result<Vec<crate::database::types::StoredTaskKnowledge>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        // Build query dynamically based on filters
        let mut sql = String::from(
            r#"SELECT id, task_run_id, category, agent_type, iteration,
                      content, evidence, confidence, related_files,
                      related_criterion_id, is_resolved, resolution_notes,
                      to_char(resolved_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                      to_char(created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
               FROM task_knowledge
               WHERE task_run_id = $1"#,
        );

        if unresolved_only {
            sql.push_str(" AND NOT is_resolved");
        }

        if !include_archived {
            sql.push_str(" AND archived_at IS NULL");
        }

        let rows = if let Some(cat) = category {
            sql.push_str(" AND category = $2");
            sql.push_str(" ORDER BY created_at ASC");
            conn.query(&sql, &[&task_run_id, &cat]).await
        } else {
            sql.push_str(" ORDER BY created_at ASC");
            conn.query(&sql, &[&task_run_id]).await
        }
        .map_err(|e| format!("PG list_task_knowledge: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| Self::row_to_stored_knowledge(r))
            .collect())
    }

    /// List reflection knowledge from previous runs of the same workflow.
    pub async fn list_workflow_knowledge(
        &self,
        workflow_name: &str,
        exclude_task_run_id: &str,
        categories: &[&str],
        limit: u32,
    ) -> Result<Vec<crate::database::types::StoredTaskKnowledge>, String> {
        if categories.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        // Build IN clause with numbered params starting from $3
        let placeholders: Vec<String> = (0..categories.len())
            .map(|i| format!("${}", i + 3))
            .collect();
        let in_clause = placeholders.join(", ");
        let limit_param = format!("${}", categories.len() + 3);

        let sql = format!(
            r#"SELECT tk.id, tk.task_run_id, tk.category, tk.agent_type, tk.iteration,
                      tk.content, tk.evidence, tk.confidence, tk.related_files,
                      tk.related_criterion_id, tk.is_resolved, tk.resolution_notes,
                      to_char(tk.resolved_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                      to_char(tk.created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
               FROM task_knowledge tk
               INNER JOIN task_runs tr ON tk.task_run_id = tr.id
               WHERE tr.workflow_name = $1
                 AND tk.task_run_id != $2
                 AND tk.agent_type = 'reflection'
                 AND tk.archived_at IS NULL
                 AND tk.category IN ({})
               ORDER BY tk.created_at DESC
               LIMIT {}"#,
            in_clause, limit_param
        );

        // Build params vector
        let limit_i64 = limit as i64;
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        params.push(&workflow_name);
        params.push(&exclude_task_run_id);
        for cat in categories {
            params.push(cat);
        }
        params.push(&limit_i64);

        let rows = conn
            .query(&sql, &params)
            .await
            .map_err(|e| format!("PG list_workflow_knowledge: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| Self::row_to_stored_knowledge(r))
            .collect())
    }

    /// Query project-scoped knowledge entries for a given project path.
    pub async fn list_project_knowledge(
        &self,
        project_path: &str,
        exclude_task_run_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;
        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"SELECT tk.category, tk.content
                   FROM task_knowledge tk
                   WHERE tk.project_path = $1
                     AND tk.task_run_id != $2
                     AND tk.agent_type = 'reflection'
                     AND tk.archived_at IS NULL
                     AND tk.category IN (
                         'project_environment', 'project_architecture',
                         'project_test_pattern', 'project_recurring_issue'
                     )
                     AND tk.confidence IN ('high', 'medium')
                   ORDER BY tk.created_at DESC
                   LIMIT $3"#,
                &[&project_path, &exclude_task_run_id, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG list_project_knowledge: {e}"))?;

        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// Query knowledge from OTHER workflows with similar names (cross-workflow learning).
    pub async fn get_cross_workflow_knowledge(
        &self,
        workflow_name: &str,
        exclude_task_run_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, String> {
        // Extract meaningful keywords from workflow name
        let keywords: Vec<&str> = workflow_name
            .split([' ', '-', '_', '>'])
            .map(|s| s.trim())
            .filter(|s| s.len() >= 3)
            .collect();

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        // Build LIKE conditions: tr.workflow_name LIKE $3, $4, ...
        let like_conditions: Vec<String> = (0..keywords.len())
            .map(|i| format!("tr.workflow_name LIKE ${}", i + 3))
            .collect();
        let where_clause = like_conditions.join(" OR ");
        let limit_param = format!("${}", keywords.len() + 3);

        let sql = format!(
            r#"SELECT DISTINCT tr.workflow_name, tk.content
               FROM task_knowledge tk
               INNER JOIN task_runs tr ON tk.task_run_id = tr.id
               WHERE ({})
                 AND tk.task_run_id != $1
                 AND tr.workflow_name != $2
                 AND tk.archived_at IS NULL
                 AND tk.category IN ('recurring_pattern', 'context', 'solution', 'root_cause')
                 AND tk.confidence IN ('high', 'medium')
               ORDER BY tk.created_at DESC
               LIMIT {}"#,
            where_clause, limit_param
        );

        let limit_i64 = limit as i64;
        let like_patterns: Vec<String> = keywords.iter().map(|k| format!("%{}%", k)).collect();

        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        params.push(&exclude_task_run_id);
        params.push(&workflow_name);
        for pattern in &like_patterns {
            params.push(pattern);
        }
        params.push(&limit_i64);

        let rows = conn
            .query(&sql, &params)
            .await
            .map_err(|e| format!("PG get_cross_workflow_knowledge: {e}"))?;

        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// Text search across task_knowledge content and category.
    /// Splits the query into keywords and matches rows where content contains ALL keywords.
    /// Returns lightweight results ordered by recency.
    pub async fn search_task_knowledge(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<(String, String, String, String, String)>, String> {
        // (id, category, content, confidence, created_at)
        let keywords: Vec<&str> = query
            .split_whitespace()
            .map(|s| s.trim())
            .filter(|s| s.len() >= 2)
            .collect();

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        // Build LIKE conditions for each keyword
        let conditions: Vec<String> = (0..keywords.len())
            .map(|i| {
                format!(
                    "(LOWER(content) LIKE ${} OR LOWER(category) LIKE ${})",
                    i + 1,
                    i + 1
                )
            })
            .collect();
        let where_clause = conditions.join(" AND ");
        let limit_idx = keywords.len() + 1;

        let sql = format!(
            r#"SELECT id, category, content, confidence, created_at::text
               FROM task_knowledge
               WHERE archived_at IS NULL AND {}
               ORDER BY created_at DESC
               LIMIT ${}"#,
            where_clause, limit_idx
        );

        let like_patterns: Vec<String> = keywords
            .iter()
            .map(|k| format!("%{}%", k.to_lowercase()))
            .collect();
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        for pattern in &like_patterns {
            params.push(pattern);
        }
        let limit_val = limit;
        params.push(&limit_val);

        let rows = conn
            .query(&sql, &params)
            .await
            .map_err(|e| format!("PG search_task_knowledge: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, String>(3),
                    r.get::<_, String>(4),
                )
            })
            .collect())
    }

    /// Helper: convert a PG row to StoredTaskKnowledge.
    fn row_to_stored_knowledge(
        row: &tokio_postgres::Row,
    ) -> crate::database::types::StoredTaskKnowledge {
        let related_files_str: Option<String> = row.get(8);
        crate::database::types::StoredTaskKnowledge {
            id: row.get(0),
            task_run_id: row.get(1),
            category: row.get(2),
            agent_type: row.get(3),
            iteration: row.get::<_, i32>(4) as u32,
            content: row.get(5),
            evidence: row.get(6),
            confidence: row.get(7),
            related_files: related_files_str
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            related_criterion_id: row.get(9),
            is_resolved: row.get(10),
            resolution_notes: row.get(11),
            resolved_at: row.get(12),
            created_at: row.get(13),
        }
    }

    /// Get task knowledge entries filtered by multiple categories.
    ///
    /// `include_archived` controls whether rows whose `archived_at` is set
    /// (i.e. compressed into a summary) are included. Pass `false` from any
    /// non-admin reader.
    pub async fn get_task_knowledge_by_categories(
        &self,
        task_run_id: &str,
        categories: &[&str],
        limit: i64,
        include_archived: bool,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        // Build IN clause dynamically
        let placeholders: Vec<String> = categories
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 2))
            .collect();
        let archived_clause = if include_archived {
            ""
        } else {
            "AND archived_at IS NULL "
        };
        let sql = format!(
            "SELECT category, content FROM task_knowledge \
             WHERE task_run_id = $1 AND category IN ({}) {}\
             ORDER BY created_at DESC LIMIT {}",
            placeholders.join(", "),
            archived_clause,
            limit
        );

        let params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> =
            std::iter::once(Box::new(task_run_id.to_string())
                as Box<dyn tokio_postgres::types::ToSql + Sync + Send>)
            .chain(categories.iter().map(|c| {
                Box::new(c.to_string()) as Box<dyn tokio_postgres::types::ToSql + Sync + Send>
            }))
            .collect();
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_task_knowledge_by_categories: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Atomically insert a compressed-summary row AND archive its source rows.
    ///
    /// Wraps the summary INSERT and the source UPDATE in a single PG
    /// transaction so a crash (or connection loss) between the two can no
    /// longer leave an orphan summary pointing at un-archived originals or
    /// archived originals whose `summary_entry_id` targets a row that was
    /// never inserted.
    ///
    /// Returns the number of source rows actually archived.
    ///
    /// Embedding the summary content is intentionally NOT done here — that's
    /// a best-effort HTTP call and has no business holding a PG transaction
    /// open. Call `embed_knowledge_content` immediately after this returns.
    #[allow(clippy::too_many_arguments)]
    pub async fn compress_and_archive(
        &self,
        summary_id: &str,
        task_run_id: &str,
        category: &str,
        agent_type: &str,
        iteration: i32,
        content: &str,
        evidence: Option<&str>,
        confidence: &str,
        related_files: &str,
        archive_ids: &[String],
    ) -> Result<usize, String> {
        let mut conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        let txn = conn
            .transaction()
            .await
            .map_err(|e| format!("PG begin compress_and_archive txn: {e}"))?;

        txn.execute(
            r#"INSERT INTO task_knowledge
               (id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
            &[
                &summary_id,
                &task_run_id,
                &category,
                &agent_type,
                &iteration,
                &content,
                &evidence,
                &confidence,
                &related_files,
            ],
        )
        .await
        .map_err(|e| format!("PG compress_and_archive insert: {e}"))?;

        let archived = if archive_ids.is_empty() {
            0
        } else {
            txn.execute(
                r#"UPDATE task_knowledge
                   SET archived_at = NOW(),
                       summary_entry_id = $1
                   WHERE id = ANY($2::text[])
                     AND archived_at IS NULL"#,
                &[&summary_id, &archive_ids],
            )
            .await
            .map_err(|e| format!("PG compress_and_archive update: {e}"))?
        };

        txn.commit()
            .await
            .map_err(|e| format!("PG commit compress_and_archive txn: {e}"))?;

        Ok(archived as usize)
    }

    /// Archive a set of knowledge entries by rolling them into a summary entry.
    ///
    /// Sets `archived_at = NOW()` and `summary_entry_id = summary_id` for every
    /// row in `ids` whose `archived_at` is currently NULL. Already-archived rows
    /// are skipped (no-op). Returns the number of rows actually updated.
    ///
    /// `summary_id` must be the id of an existing `task_knowledge` row — the
    /// foreign key on `summary_entry_id` enforces this.
    pub async fn archive_task_knowledge(
        &self,
        ids: &[String],
        summary_id: &str,
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;

        let updated = conn
            .execute(
                r#"UPDATE task_knowledge
                   SET archived_at = NOW(),
                       summary_entry_id = $1
                   WHERE id = ANY($2::text[])
                     AND archived_at IS NULL"#,
                &[&summary_id, &ids],
            )
            .await
            .map_err(|e| format!("PG archive_task_knowledge: {e}"))?;

        Ok(updated as usize)
    }
}
