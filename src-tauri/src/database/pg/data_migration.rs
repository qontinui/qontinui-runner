//! One-time data migration from SQLite to PostgreSQL.
//!
//! Copies all rows from SQLite tables that have corresponding PG tables.
//! Runs on startup after PG connection is established.
//! Each table is migrated independently and idempotently (ON CONFLICT DO NOTHING).
//! Tables that already have data in PG are skipped.

use super::PgDb;
use crate::database::CheckpointDb;
use tracing::{info, warn};

/// Statistics from the migration run.
#[derive(Default, Debug)]
pub struct MigrationStats {
    pub tables_migrated: u32,
    pub tables_skipped: u32,
    pub rows_migrated: u64,
    pub errors: Vec<String>,
}

impl std::fmt::Display for MigrationStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tables_migrated={}, tables_skipped={}, rows_migrated={}, errors={}",
            self.tables_migrated, self.tables_skipped, self.rows_migrated, self.errors.len()
        )
    }
}

/// Helper: check if a PG table already has data.
async fn pg_table_has_data(
    conn: &deadpool_postgres::Object,
    table: &str,
) -> Result<bool, String> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)", table);
    let row = conn
        .query_one(&sql as &str, &[])
        .await
        .map_err(|e| format!("PG check {}: {}", table, e))?;
    let exists: bool = row.get(0);
    Ok(exists)
}

impl PgDb {
    /// One-time migration of all data from SQLite to PostgreSQL.
    /// Skips tables that already have data in PG.
    /// Called on startup after PG connection is established.
    pub async fn migrate_all_from_sqlite(
        &self,
        sqlite: &CheckpointDb,
    ) -> Result<MigrationStats, String> {
        let mut stats = MigrationStats::default();

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Quick check: if task_runs already has data, migration already ran
        if pg_table_has_data(&conn, "task_runs").await.unwrap_or(false) {
            let count: i64 = conn
                .query_one("SELECT COUNT(*) FROM task_runs", &[])
                .await
                .map_err(|e| format!("PG count: {}", e))?
                .get(0);
            info!(
                "PG already has {} task_runs — skipping SQLite→PG data migration",
                count
            );
            return Ok(stats);
        }

        info!("Starting SQLite → PG data migration...");

        // Verify PG connectivity
        match conn.query_one("SELECT current_setting('search_path')", &[]).await {
            Ok(row) => {
                let sp: String = row.get(0);
                info!("Migration PG search_path: {}", sp);
            }
            Err(e) => warn!("Migration PG search_path check failed: {}", e),
        }

        // Count existing PG tables
        match conn.query_one(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = 'runner'",
            &[],
        ).await {
            Ok(row) => {
                let count: i64 = row.get(0);
                info!("Migration: PG has {} tables in runner schema", count);
            }
            Err(e) => warn!("Migration: table count check failed: {}", e),
        }

        // =====================================================================
        // Tier 1: Tables with no foreign key dependencies
        // =====================================================================

        self.migrate_settings(sqlite, &mut stats).await;
        self.migrate_configs(sqlite, &mut stats).await;
        self.migrate_shell_commands(sqlite, &mut stats).await;
        self.migrate_mcp_servers(sqlite, &mut stats).await;
        self.migrate_saved_api_requests(sqlite, &mut stats).await;
        self.migrate_checks(sqlite, &mut stats).await;
        self.migrate_check_groups(sqlite, &mut stats).await;
        self.migrate_user_skills(sqlite, &mut stats).await;
        self.migrate_learning_patterns(sqlite, &mut stats).await;
        self.migrate_learning_outcomes(sqlite, &mut stats).await;
        self.migrate_unified_workflows_no_fk(sqlite, &mut stats).await;
        self.migrate_stall_events(sqlite, &mut stats).await;

        // =====================================================================
        // Tier 2: task_runs (self-referencing — parents first)
        // =====================================================================

        self.migrate_task_runs(sqlite, &mut stats).await;

        // Now back-fill unified_workflows.generated_by_task_run_id
        self.backfill_unified_workflow_fks(sqlite, &mut stats).await;

        // =====================================================================
        // Tier 2b: Tables that depend on task_runs
        // =====================================================================

        self.migrate_phase_token_usage(sqlite, &mut stats).await;
        self.migrate_task_run_events(sqlite, &mut stats).await;
        self.migrate_task_run_findings(sqlite, &mut stats).await;
        self.migrate_task_run_screenshots(sqlite, &mut stats).await;
        self.migrate_workflow_step_checkpoints(sqlite, &mut stats).await;
        self.migrate_workflow_execution_state(sqlite, &mut stats).await;
        self.migrate_approval_gates(sqlite, &mut stats).await;
        self.migrate_ui_bridge_elements(sqlite, &mut stats).await;
        self.migrate_ui_bridge_events(sqlite, &mut stats).await;

        // =====================================================================
        // Tier 3: Tables that depend on tier 2 tables
        // =====================================================================

        self.migrate_check_group_members(sqlite, &mut stats).await;
        self.migrate_step_progress_markers(sqlite, &mut stats).await;

        info!(
            "SQLite → PG data migration complete: {}",
            stats
        );

        if !stats.errors.is_empty() {
            warn!(
                "Migration had {} errors: {:?}",
                stats.errors.len(),
                &stats.errors[..stats.errors.len().min(5)]
            );
        }

        Ok(stats)
    }

    // =========================================================================
    // Tier 1 table migrations
    // =========================================================================

    async fn migrate_settings(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "settings";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare("SELECT key, value, updated_at FROM settings")
                .map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for (key, value, updated_at) in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3::timestamptz) ON CONFLICT DO NOTHING",
                &[key, value, updated_at],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_configs(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "configs";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, config_json, source_type, source_path, created_at, updated_at FROM configs"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for row in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO configs (id, name, config_json, source_type, source_path, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6::timestamptz, $7::timestamptz) ON CONFLICT DO NOTHING",
                &[&row.0, &row.1, &row.2, &row.3, &row.4, &row.5, &row.6],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_shell_commands(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "shell_commands";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, description, command, working_directory, timeout_seconds, \
                 fail_on_error, category, tags, enabled, created_at, updated_at FROM shell_commands"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i32>>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, bool>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO shell_commands (id, name, description, command, working_directory, \
                 timeout_seconds, fail_on_error, category, tags, enabled, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::timestamptz,$12::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9, &r.10, &r.11],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_mcp_servers(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "mcp_servers";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, description, transport, stdio_config, http_config, \
                 enabled, auto_start, timeout_seconds, cached_tools, tools_cached_at, \
                 created_at, updated_at FROM mcp_servers"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, i32>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO mcp_servers (id, name, description, transport, stdio_config, http_config, \
                 enabled, auto_start, timeout_seconds, cached_tools, tools_cached_at, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12::timestamptz,$13::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9, &r.10, &r.11, &r.12],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_saved_api_requests(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "saved_api_requests";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, description, category, tags, method, url, headers, body, \
                 body_content_type, timeout_ms, follow_redirects, variable_extractions, \
                 assertions, credential_id, created_at, updated_at FROM saved_api_requests"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<i32>>(10)?,
                    row.get::<_, bool>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO saved_api_requests (id, name, description, category, tags, method, url, \
                 headers, body, body_content_type, timeout_ms, follow_redirects, variable_extractions, \
                 assertions, credential_id, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16::timestamptz,$17::timestamptz) \
                 ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9,
                  &r.10, &r.11, &r.12, &r.13, &r.14, &r.15, &r.16],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_checks(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "checks";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, description, check_type, tool, command, working_directory, \
                 config_path, auto_fix, fail_on_warning, timeout_seconds, is_critical, enabled, \
                 ai_generated, ai_generation_prompt, tags, created_at, updated_at FROM checks"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok(CheckRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    check_type: row.get(3)?,
                    tool: row.get(4)?,
                    command: row.get(5)?,
                    working_directory: row.get(6)?,
                    config_path: row.get(7)?,
                    auto_fix: row.get(8)?,
                    fail_on_warning: row.get(9)?,
                    timeout_seconds: row.get(10)?,
                    is_critical: row.get(11)?,
                    enabled: row.get(12)?,
                    ai_generated: row.get(13)?,
                    ai_generation_prompt: row.get(14)?,
                    tags: row.get(15)?,
                    created_at: row.get(16)?,
                    updated_at: row.get(17)?,
                })
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO checks (id, name, description, check_type, tool, command, working_directory, \
                 config_path, auto_fix, fail_on_warning, timeout_seconds, is_critical, enabled, \
                 ai_generated, ai_generation_prompt, tags, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17::timestamptz,$18::timestamptz) \
                 ON CONFLICT DO NOTHING",
                &[&r.id, &r.name, &r.description, &r.check_type, &r.tool, &r.command,
                  &r.working_directory, &r.config_path, &r.auto_fix, &r.fail_on_warning,
                  &r.timeout_seconds, &r.is_critical, &r.enabled, &r.ai_generated,
                  &r.ai_generation_prompt, &r.tags, &r.created_at, &r.updated_at],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_check_groups(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "check_groups";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, description, color, enabled, run_in_parallel, stop_on_failure, \
                 tags, created_at, updated_at FROM check_groups"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO check_groups (id, name, description, color, enabled, run_in_parallel, \
                 stop_on_failure, tags, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::timestamptz,$10::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_user_skills(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "user_skills";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, slug, description, category, tags, icon, color, allowed_phases, \
                 parameters, template, source, version, author, checksum, depends_on, usage_count, \
                 approval_status, forked_from, source_fix_id, source_pattern_id, created_at, updated_at \
                 FROM user_skills"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok(UserSkillRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get(3)?,
                    category: row.get(4)?,
                    tags: row.get(5)?,
                    icon: row.get(6)?,
                    color: row.get(7)?,
                    allowed_phases: row.get(8)?,
                    parameters: row.get(9)?,
                    template: row.get(10)?,
                    source: row.get(11)?,
                    version: row.get(12)?,
                    author: row.get(13)?,
                    checksum: row.get(14)?,
                    depends_on: row.get(15)?,
                    usage_count: row.get(16)?,
                    approval_status: row.get(17)?,
                    forked_from: row.get(18)?,
                    source_fix_id: row.get(19)?,
                    source_pattern_id: row.get(20)?,
                    created_at: row.get(21)?,
                    updated_at: row.get(22)?,
                })
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let usage_count = r.usage_count.map(|v| v as i64);
            if let Err(e) = conn.execute(
                "INSERT INTO user_skills (id, name, slug, description, category, tags, icon, color, \
                 allowed_phases, parameters, template, source, version, author, checksum, depends_on, \
                 usage_count, approval_status, forked_from, source_fix_id, source_pattern_id, \
                 created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21, \
                 $22::timestamptz,$23::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.id, &r.name, &r.slug, &r.description, &r.category, &r.tags,
                  &r.icon, &r.color, &r.allowed_phases, &r.parameters, &r.template,
                  &r.source, &r.version, &r.author, &r.checksum, &r.depends_on,
                  &usage_count, &r.approval_status, &r.forked_from, &r.source_fix_id,
                  &r.source_pattern_id, &r.created_at, &r.updated_at],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_learning_patterns(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "learning_patterns";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, pattern_type, description, confidence, occurrences, context, \
                 created_at, updated_at FROM learning_patterns"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i32>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, \
                 context, created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7::timestamptz,$8::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_learning_outcomes(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "learning_outcomes";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, task_id, status, duration_secs, iterations, strategy, tools_used, \
                 files_modified, error_type, error_message, feedback, workflow_architecture, \
                 step_count, verification_step_count, agentic_step_count, has_ui_bridge, \
                 total_tokens, total_cost_usd, composite_agentic_score, technology_tags, \
                 domain_tags, complexity_tier, created_at FROM learning_outcomes"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok(LearningOutcomeRow {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    status: row.get(2)?,
                    duration_secs: row.get(3)?,
                    iterations: row.get(4)?,
                    strategy: row.get(5)?,
                    tools_used: row.get(6)?,
                    files_modified: row.get(7)?,
                    error_type: row.get(8)?,
                    error_message: row.get(9)?,
                    feedback: row.get(10)?,
                    workflow_architecture: row.get(11)?,
                    step_count: row.get(12)?,
                    verification_step_count: row.get(13)?,
                    agentic_step_count: row.get(14)?,
                    has_ui_bridge: row.get(15)?,
                    total_tokens: row.get(16)?,
                    total_cost_usd: row.get(17)?,
                    composite_agentic_score: row.get(18)?,
                    technology_tags: row.get(19)?,
                    domain_tags: row.get(20)?,
                    complexity_tier: row.get(21)?,
                    created_at: row.get(22)?,
                })
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let step_count = r.step_count.map(|v| v as i64);
            let ver_step = r.verification_step_count.map(|v| v as i64);
            let ag_step = r.agentic_step_count.map(|v| v as i64);
            let has_ui = r.has_ui_bridge.unwrap_or(false);
            let total_tok = r.total_tokens.map(|v| v as i64);
            if let Err(e) = conn.execute(
                "INSERT INTO learning_outcomes (id, task_id, status, duration_secs, iterations, \
                 strategy, tools_used, files_modified, error_type, error_message, feedback, \
                 workflow_architecture, step_count, verification_step_count, agentic_step_count, \
                 has_ui_bridge, total_tokens, total_cost_usd, composite_agentic_score, \
                 technology_tags, domain_tags, complexity_tier, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23::timestamptz) \
                 ON CONFLICT DO NOTHING",
                &[&r.id, &r.task_id, &r.status, &r.duration_secs, &r.iterations,
                  &r.strategy, &r.tools_used, &r.files_modified, &r.error_type, &r.error_message,
                  &r.feedback, &r.workflow_architecture, &step_count, &ver_step, &ag_step,
                  &has_ui, &total_tok, &r.total_cost_usd, &r.composite_agentic_score,
                  &r.technology_tags, &r.domain_tags, &r.complexity_tier, &r.created_at],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    /// Migrate unified_workflows without the generated_by_task_run_id FK
    /// (task_runs haven't been migrated yet). We back-fill the FK after task_runs.
    async fn migrate_unified_workflows_no_fk(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "unified_workflows";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, description, category, tags, setup_steps, verification_steps, \
                 agentic_steps, completion_steps, max_iterations, provider, model, skip_ai_summary, \
                 timeout_seconds, prompt_template, context_ids, disabled_context_ids, \
                 auto_include_contexts, log_watch_enabled, log_source_selection, \
                 health_check_enabled, health_check_urls, preflight_check_enabled, enable_sweep, \
                 max_sweep_iterations, stages, stop_on_failure, constraint_overrides, approval_gate, \
                 reflection_mode, completion_prompts_first, model_overrides, example_status, \
                 sync_pending, is_favorite, dependency_graph, cost_annotations, quality_report, \
                 acceptance_criteria, ai_reviewed, workflow_architecture, source_file_path, \
                 source_content_hash, rollback_policy, strict_cwd, tool_tags, enforce_token_budget, \
                 flow_control_json, phase_timeouts_json, created_at, updated_at \
                 FROM unified_workflows"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok(UnifiedWorkflowRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    category: row.get(3)?,
                    tags: row.get(4)?,
                    setup_steps: row.get(5)?,
                    verification_steps: row.get(6)?,
                    agentic_steps: row.get(7)?,
                    completion_steps: row.get(8)?,
                    max_iterations: row.get(9)?,
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get(12)?,
                    timeout_seconds: row.get(13)?,
                    prompt_template: row.get(14)?,
                    context_ids: row.get(15)?,
                    disabled_context_ids: row.get(16)?,
                    auto_include_contexts: row.get(17)?,
                    log_watch_enabled: row.get(18)?,
                    log_source_selection: row.get(19)?,
                    health_check_enabled: row.get(20)?,
                    health_check_urls: row.get(21)?,
                    preflight_check_enabled: row.get(22)?,
                    enable_sweep: row.get(23)?,
                    max_sweep_iterations: row.get(24)?,
                    stages: row.get(25)?,
                    stop_on_failure: row.get(26)?,
                    constraint_overrides: row.get(27)?,
                    approval_gate: row.get(28)?,
                    reflection_mode: row.get(29)?,
                    completion_prompts_first: row.get(30)?,
                    model_overrides: row.get(31)?,
                    example_status: row.get(32)?,
                    sync_pending: row.get(33)?,
                    is_favorite: row.get(34)?,
                    dependency_graph: row.get(35)?,
                    cost_annotations: row.get(36)?,
                    quality_report: row.get(37)?,
                    acceptance_criteria: row.get(38)?,
                    ai_reviewed: row.get(39)?,
                    workflow_architecture: row.get(40)?,
                    source_file_path: row.get(41)?,
                    source_content_hash: row.get(42)?,
                    rollback_policy: row.get(43)?,
                    strict_cwd: row.get(44)?,
                    tool_tags: row.get(45)?,
                    enforce_token_budget: row.get(46)?,
                    flow_control_json: row.get(47)?,
                    phase_timeouts_json: row.get(48)?,
                    created_at: row.get(49)?,
                    updated_at: row.get(50)?,
                })
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let max_iter = r.max_iterations.map(|v| v as i64);
            let timeout = r.timeout_seconds.map(|v| v as i64);
            let auto_inc = r.auto_include_contexts.unwrap_or(true);
            let log_watch = r.log_watch_enabled.unwrap_or(true);
            let health_check = r.health_check_enabled.unwrap_or(true);
            let preflight = r.preflight_check_enabled.unwrap_or(true);
            let sweep = r.enable_sweep.unwrap_or(false);
            let max_sweep = r.max_sweep_iterations.map(|v| v as i64);
            let stop_fail = r.stop_on_failure.unwrap_or(false);
            let approval = r.approval_gate.unwrap_or(false);
            let reflection = r.reflection_mode.unwrap_or(true);
            let comp_first = r.completion_prompts_first;
            let sync = r.sync_pending.unwrap_or(false);
            let fav = r.is_favorite.unwrap_or(false);
            let ai_rev = r.ai_reviewed.unwrap_or(true);
            let strict = r.strict_cwd.unwrap_or(false);
            let enforce_tok = r.enforce_token_budget.unwrap_or(false);

            if let Err(e) = conn.execute(
                "INSERT INTO unified_workflows (id, name, description, category, tags, setup_steps, \
                 verification_steps, agentic_steps, completion_steps, max_iterations, provider, model, \
                 skip_ai_summary, timeout_seconds, prompt_template, context_ids, disabled_context_ids, \
                 auto_include_contexts, log_watch_enabled, log_source_selection, health_check_enabled, \
                 health_check_urls, preflight_check_enabled, enable_sweep, max_sweep_iterations, \
                 stages, stop_on_failure, constraint_overrides, approval_gate, reflection_mode, \
                 completion_prompts_first, model_overrides, example_status, sync_pending, is_favorite, \
                 dependency_graph, cost_annotations, quality_report, acceptance_criteria, ai_reviewed, \
                 workflow_architecture, source_file_path, source_content_hash, rollback_policy, \
                 strict_cwd, tool_tags, enforce_token_budget, flow_control_json, phase_timeouts_json, \
                 created_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20, \
                 $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37,$38,$39,$40, \
                 $41,$42,$43,$44,$45,$46,$47,$48,$49,$50::timestamptz,$51::timestamptz) \
                 ON CONFLICT DO NOTHING",
                &[
                    &r.id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &r.name, &r.description, &r.category, &r.tags,
                    &r.setup_steps, &r.verification_steps, &r.agentic_steps, &r.completion_steps,
                    &max_iter, &r.provider, &r.model, &r.skip_ai_summary,
                    &timeout, &r.prompt_template, &r.context_ids, &r.disabled_context_ids,
                    &auto_inc, &log_watch, &r.log_source_selection,
                    &health_check, &r.health_check_urls, &preflight,
                    &sweep, &max_sweep, &r.stages, &stop_fail,
                    &r.constraint_overrides, &approval, &reflection,
                    &comp_first, &r.model_overrides, &r.example_status,
                    &sync, &fav, &r.dependency_graph, &r.cost_annotations,
                    &r.quality_report, &r.acceptance_criteria, &ai_rev,
                    &r.workflow_architecture, &r.source_file_path, &r.source_content_hash,
                    &r.rollback_policy, &strict, &r.tool_tags, &enforce_tok,
                    &r.flow_control_json, &r.phase_timeouts_json,
                    &r.created_at, &r.updated_at,
                ],
            ).await {
                stats.errors.push(format!("{}: insert {}: {}", table, r.id, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_stall_events(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "stall_events";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        // stall_events may not exist in older SQLite databases
        let rows = match sqlite.with_conn(|c| {
            let mut stmt = match c.prepare(
                "SELECT id, task_run_id, iteration, pattern_type, pattern_details, action_count, \
                 intervention_action, intervention_result, created_at FROM stall_events"
            ) {
                Ok(s) => s,
                Err(_) => return Ok(Vec::new()), // table doesn't exist
            };
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i32>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO stall_events (id, task_run_id, iteration, pattern_type, pattern_details, \
                 action_count, intervention_action, intervention_result, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    // =========================================================================
    // Tier 2: task_runs (self-referencing — insert parents first)
    // =========================================================================

    async fn migrate_task_runs(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "task_runs";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        // Read all task_runs ordered by depth (parents before children)
        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, \
                 auto_continue, error_message, execution_steps_json, log_sources_json, \
                 config_id, workflow_name, workflow_id, summary, ai_summary, goal_achieved, \
                 remaining_work, summary_generated_at, runtime_context_json, transition_history_json, \
                 parent_task_run_id, root_task_run_id, depth, bridge_id, workflow_type, result_data, \
                 workspace_id, triggered_by, is_reflection, reflection_source_task_run_id, \
                 is_follow_up, follow_up_source_task_run_id, runner_port, is_fixer, \
                 fixer_source_task_run_id, is_meta_optimizer, iteration_history, pipeline_checkpoint, \
                 iteration_diffs, iteration_commits, verification_passed, \
                 total_input_tokens, total_output_tokens, total_cost_cents, \
                 created_at, updated_at, completed_at \
                 FROM task_runs ORDER BY depth ASC, created_at ASC"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok(TaskRunRow {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row.get(3)?,
                    status: row.get(4)?,
                    sessions_count: row.get(5)?,
                    max_sessions: row.get(6)?,
                    auto_continue: row.get(7)?,
                    error_message: row.get(8)?,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get(16)?,
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    runtime_context_json: row.get(19)?,
                    transition_history_json: row.get(20)?,
                    parent_task_run_id: row.get(21)?,
                    root_task_run_id: row.get(22)?,
                    depth: row.get(23)?,
                    bridge_id: row.get(24)?,
                    workflow_type: row.get(25)?,
                    result_data: row.get(26)?,
                    workspace_id: row.get(27)?,
                    triggered_by: row.get(28)?,
                    is_reflection: row.get(29)?,
                    reflection_source_task_run_id: row.get(30)?,
                    is_follow_up: row.get(31)?,
                    follow_up_source_task_run_id: row.get(32)?,
                    runner_port: row.get(33)?,
                    is_fixer: row.get(34)?,
                    fixer_source_task_run_id: row.get(35)?,
                    is_meta_optimizer: row.get(36)?,
                    iteration_history: row.get(37)?,
                    pipeline_checkpoint: row.get(38)?,
                    iteration_diffs: row.get(39)?,
                    iteration_commits: row.get(40)?,
                    verification_passed: row.get(41)?,
                    total_input_tokens: row.get(42)?,
                    total_output_tokens: row.get(43)?,
                    total_cost_cents: row.get(44)?,
                    created_at: row.get(45)?,
                    updated_at: row.get(46)?,
                    completed_at: row.get(47)?,
                })
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let sessions_count = r.sessions_count as i32;
            let max_sessions: Option<i32> = r.max_sessions.map(|v| v as i32);
            let auto_continue = r.auto_continue;
            let depth = r.depth.unwrap_or(0) as i32;
            let is_reflection = int_to_bool(r.is_reflection);
            let is_follow_up = int_to_bool(r.is_follow_up);
            let is_fixer = int_to_bool(r.is_fixer);
            let is_meta_optimizer = int_to_bool(r.is_meta_optimizer);
            let verification_passed = r.verification_passed.map(|v| v != 0);
            let total_input = r.total_input_tokens.unwrap_or(0) as i64;
            let total_output = r.total_output_tokens.unwrap_or(0) as i64;
            let total_cost = r.total_cost_cents.unwrap_or(0) as i64;
            let runner_port: Option<i32> = r.runner_port.map(|v| v as i32);

            if let Err(e) = conn.execute(
                "INSERT INTO task_runs (id, task_name, prompt, task_type, status, sessions_count, \
                 max_sessions, auto_continue, error_message, execution_steps_json, log_sources_json, \
                 config_id, workflow_name, workflow_id, summary, ai_summary, goal_achieved, \
                 remaining_work, summary_generated_at, runtime_context_json, transition_history_json, \
                 parent_task_run_id, root_task_run_id, depth, bridge_id, workflow_type, result_data, \
                 workspace_id, triggered_by, is_reflection, reflection_source_task_run_id, \
                 is_follow_up, follow_up_source_task_run_id, runner_port, is_fixer, \
                 fixer_source_task_run_id, is_meta_optimizer, iteration_history, pipeline_checkpoint, \
                 iteration_diffs, iteration_commits, verification_passed, \
                 total_input_tokens, total_output_tokens, total_cost_cents, \
                 created_at, updated_at, completed_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20, \
                 $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37,$38,$39,$40, \
                 $41,$42,$43,$44,$45,$46::timestamptz,$47::timestamptz,$48::timestamptz) \
                 ON CONFLICT DO NOTHING",
                &[
                    &r.id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &r.task_name, &r.prompt, &r.task_type, &r.status,
                    &sessions_count, &max_sessions, &auto_continue,
                    &r.error_message, &r.execution_steps_json, &r.log_sources_json,
                    &r.config_id, &r.workflow_name, &r.workflow_id,
                    &r.summary, &r.ai_summary, &r.goal_achieved,
                    &r.remaining_work, &r.summary_generated_at, &r.runtime_context_json,
                    &r.transition_history_json, &r.parent_task_run_id, &r.root_task_run_id,
                    &depth, &r.bridge_id, &r.workflow_type, &r.result_data,
                    &r.workspace_id, &r.triggered_by, &is_reflection,
                    &r.reflection_source_task_run_id, &is_follow_up,
                    &r.follow_up_source_task_run_id, &runner_port, &is_fixer,
                    &r.fixer_source_task_run_id, &is_meta_optimizer,
                    &r.iteration_history, &r.pipeline_checkpoint,
                    &r.iteration_diffs, &r.iteration_commits, &verification_passed,
                    &total_input, &total_output, &total_cost,
                    &r.created_at, &r.updated_at, &r.completed_at,
                ],
            ).await {
                stats.errors.push(format!("{}: insert {}: {}", table, r.id, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    /// Back-fill generated_by_task_run_id on unified_workflows now that task_runs exist.
    async fn backfill_unified_workflow_fks(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("backfill_uw_fk: pool: {}", e)); return; }
        };

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, generated_by_task_run_id FROM unified_workflows WHERE generated_by_task_run_id IS NOT NULL"
            ).map_err(|e| format!("backfill_uw_fk: {}", e))?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }).map_err(|e| format!("backfill_uw_fk: {}", e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("backfill_uw_fk: {}", e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        for (wf_id, tr_id) in &rows {
            let _ = conn.execute(
                "UPDATE unified_workflows SET generated_by_task_run_id = $1 WHERE id = $2",
                &[tr_id, wf_id],
            ).await;
        }
    }

    // =========================================================================
    // Tier 2b: Tables that depend on task_runs
    // =========================================================================

    async fn migrate_phase_token_usage(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "phase_token_usage";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT task_run_id, phase, stage_index, iteration, model_used, provider_used, \
                 input_tokens, output_tokens, cost_cents, duration_ms, target_app, target_page_url, \
                 created_at FROM phase_token_usage"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i32>>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, String>(12)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO phase_token_usage (task_run_id, phase, stage_index, iteration, model_used, \
                 provider_used, input_tokens, output_tokens, cost_cents, duration_ms, target_app, \
                 target_page_url, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13::timestamptz)",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9, &r.10, &r.11, &r.12],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_task_run_events(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "task_run_events";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT task_run_id, event_type, event_subtype, message, data, workflow_name, \
                 state_name, action_id, timestamp, duration_ms FROM task_run_events"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO task_run_events (task_run_id, event_type, event_subtype, message, data, \
                 workflow_name, state_name, action_id, timestamp, duration_ms) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_task_run_findings(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "task_run_findings";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, task_run_id, category, severity, signature_hash, title, description, \
                 file_path, line_number, column_number, code_snippet, status, action_type, resolution, \
                 detected_in_session, resolved_in_session, needs_input, question, input_options, \
                 user_response, reflection_fix_id, detected_at, resolved_at, updated_at \
                 FROM task_run_findings"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok(FindingRow {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    category: row.get(2)?,
                    severity: row.get(3)?,
                    signature_hash: row.get(4)?,
                    title: row.get(5)?,
                    description: row.get(6)?,
                    file_path: row.get(7)?,
                    line_number: row.get(8)?,
                    column_number: row.get(9)?,
                    code_snippet: row.get(10)?,
                    status: row.get(11)?,
                    action_type: row.get(12)?,
                    resolution: row.get(13)?,
                    detected_in_session: row.get(14)?,
                    resolved_in_session: row.get(15)?,
                    needs_input: row.get(16)?,
                    question: row.get(17)?,
                    input_options: row.get(18)?,
                    user_response: row.get(19)?,
                    reflection_fix_id: row.get(20)?,
                    detected_at: row.get(21)?,
                    resolved_at: row.get(22)?,
                    updated_at: row.get(23)?,
                })
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let needs_input = r.needs_input.unwrap_or(false);
            if let Err(e) = conn.execute(
                "INSERT INTO task_run_findings (id, task_run_id, category, severity, signature_hash, \
                 title, description, file_path, line_number, column_number, code_snippet, status, \
                 action_type, resolution, detected_in_session, resolved_in_session, needs_input, \
                 question, input_options, user_response, reflection_fix_id, detected_at, resolved_at, \
                 updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21, \
                 $22::timestamptz,$23::timestamptz,$24::timestamptz) ON CONFLICT DO NOTHING",
                &[
                    &r.id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &r.task_run_id, &r.category, &r.severity, &r.signature_hash,
                    &r.title, &r.description, &r.file_path, &r.line_number, &r.column_number,
                    &r.code_snippet, &r.status, &r.action_type, &r.resolution,
                    &r.detected_in_session, &r.resolved_in_session, &needs_input,
                    &r.question, &r.input_options, &r.user_response, &r.reflection_fix_id,
                    &r.detected_at, &r.resolved_at, &r.updated_at,
                ],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_task_run_screenshots(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "task_run_screenshots";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, task_run_id, file_path, screenshot_type, template_name, confidence, \
                 match_location, width, height, file_size_bytes, created_at \
                 FROM task_run_screenshots"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<f64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i32>>(7)?,
                    row.get::<_, Option<i32>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO task_run_screenshots (id, task_run_id, file_path, screenshot_type, \
                 template_name, confidence, match_location, width, height, file_size_bytes, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9, &r.10],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_workflow_step_checkpoints(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "workflow_step_checkpoints";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, execution_id, workflow_type, phase, iteration, step_index, step_type, \
                 step_name, status, result_json, step_config_json, started_at, completed_at, \
                 duration_ms, error, stage_index FROM workflow_step_checkpoints"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i32>>(4)?,
                    row.get::<_, i32>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<i32>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<i32>>(15)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO workflow_step_checkpoints (id, execution_id, workflow_type, phase, iteration, \
                 step_index, step_type, step_name, status, result_json, step_config_json, started_at, \
                 completed_at, duration_ms, error, stage_index) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9,
                  &r.10, &r.11, &r.12, &r.13, &r.14, &r.15],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_workflow_execution_state(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "workflow_execution_state";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT execution_id, workflow_type, state_name, state_data, phase, iteration, \
                 updated_at FROM workflow_execution_state"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i32>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO workflow_execution_state (execution_id, workflow_type, state_name, state_data, \
                 phase, iteration, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_approval_gates(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "approval_gates";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, task_run_id, iteration, prompt, context_json, action, comment, status, \
                 created_at, resolved_at FROM approval_gates"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO approval_gates (id, task_run_id, iteration, prompt, context_json, action, \
                 comment, status, created_at, resolved_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::timestamptz,$10::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_ui_bridge_elements(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "ui_bridge_elements";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT task_run_id, timestamp, element_id, tag_name, element_type, bounds, \
                 visible, enabled, focused, value, text_content, label, metadata \
                 FROM ui_bridge_elements"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<bool>>(6)?,
                    row.get::<_, Option<bool>>(7)?,
                    row.get::<_, Option<bool>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let visible = r.5.as_ref().map(|_| r.6.unwrap_or(true)).unwrap_or(true);
            let enabled = r.7.unwrap_or(true);
            let focused = r.8.unwrap_or(false);
            if let Err(e) = conn.execute(
                "INSERT INTO ui_bridge_elements (task_run_id, timestamp, element_id, tag_name, \
                 element_type, bounds, visible, enabled, focused, value, text_content, label, metadata) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &visible, &enabled, &focused,
                  &r.9, &r.10, &r.11, &r.12],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_ui_bridge_events(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "ui_bridge_events";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT task_run_id, timestamp, sequence, event_type, element_id, state_id, \
                 transition_id, action, params, result, duration_ms, success, error_message, metadata \
                 FROM ui_bridge_events"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<f64>>(10)?,
                    row.get::<_, Option<bool>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            let success = r.11.unwrap_or(true);
            if let Err(e) = conn.execute(
                "INSERT INTO ui_bridge_events (task_run_id, timestamp, sequence, event_type, element_id, \
                 state_id, transition_id, action, params, result, duration_ms, success, error_message, metadata) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6, &r.7, &r.8, &r.9, &r.10, &success, &r.12, &r.13],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    // =========================================================================
    // Tier 3: Tables that depend on tier 2
    // =========================================================================

    async fn migrate_check_group_members(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "check_group_members";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, group_id, check_id, sort_order, created_at FROM check_group_members"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i32>(3)?,
                    row.get::<_, String>(4)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO check_group_members (id, group_id, check_id, sort_order, created_at) \
                 VALUES ($1,$2,$3,$4,$5::timestamptz) ON CONFLICT DO NOTHING",
                &[&r.0, &r.1, &r.2, &r.3, &r.4],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }

    async fn migrate_step_progress_markers(&self, sqlite: &CheckpointDb, stats: &mut MigrationStats) {
        let table = "step_progress_markers";
        let conn = match self.pool().get().await {
            Ok(c) => c,
            Err(e) => { stats.errors.push(format!("{}: pool error: {}", table, e)); return; }
        };
        if pg_table_has_data(&conn, table).await.unwrap_or(false) {
            stats.tables_skipped += 1;
            return;
        }

        let rows = match sqlite.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT checkpoint_id, marker_type, current_value, total_value, description, \
                 data_json, created_at FROM step_progress_markers"
            ).map_err(|e| format!("{}: {}", table, e))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            }).map_err(|e| format!("{}: {}", table, e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{}: {}", table, e))
        }) {
            Ok(r) => r,
            Err(e) => { stats.errors.push(e); return; }
        };

        let mut count = 0u64;
        for r in &rows {
            if let Err(e) = conn.execute(
                "INSERT INTO step_progress_markers (checkpoint_id, marker_type, current_value, \
                 total_value, description, data_json, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7::timestamptz)",
                &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6],
            ).await {
                stats.errors.push(format!("{}: insert: {}", table, e));
                continue;
            }
            count += 1;
        }
        if count > 0 {
            info!("Migrated {} rows from {}", count, table);
            stats.tables_migrated += 1;
            stats.rows_migrated += count;
        }
    }
}

// =============================================================================
// Helper types for rows with many columns (avoids deeply nested tuples)
// =============================================================================

/// Convert SQLite INTEGER boolean (0/1/None) to Option<bool>
fn int_to_bool(v: Option<i32>) -> Option<bool> {
    v.map(|i| i != 0)
}

struct CheckRow {
    id: String,
    name: String,
    description: Option<String>,
    check_type: String,
    tool: String,
    command: Option<String>,
    working_directory: Option<String>,
    config_path: Option<String>,
    auto_fix: bool,
    fail_on_warning: bool,
    timeout_seconds: Option<i32>,
    is_critical: bool,
    enabled: bool,
    ai_generated: bool,
    ai_generation_prompt: Option<String>,
    tags: Option<String>,
    created_at: String,
    updated_at: String,
}

struct UserSkillRow {
    id: String,
    name: String,
    slug: String,
    description: Option<String>,
    category: Option<String>,
    tags: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    allowed_phases: String,
    parameters: Option<String>,
    template: String,
    source: String,
    version: Option<String>,
    author: Option<String>,
    checksum: Option<String>,
    depends_on: Option<String>,
    usage_count: Option<i32>,
    approval_status: Option<String>,
    forked_from: Option<String>,
    source_fix_id: Option<String>,
    source_pattern_id: Option<String>,
    created_at: String,
    updated_at: String,
}

struct LearningOutcomeRow {
    id: String,
    task_id: String,
    status: String,
    duration_secs: Option<f64>,
    iterations: Option<i32>,
    strategy: Option<String>,
    tools_used: Option<String>,
    files_modified: Option<String>,
    error_type: Option<String>,
    error_message: Option<String>,
    feedback: Option<String>,
    workflow_architecture: Option<String>,
    step_count: Option<i32>,
    verification_step_count: Option<i32>,
    agentic_step_count: Option<i32>,
    has_ui_bridge: Option<bool>,
    total_tokens: Option<i32>,
    total_cost_usd: Option<f64>,
    composite_agentic_score: Option<f64>,
    technology_tags: Option<String>,
    domain_tags: Option<String>,
    complexity_tier: Option<String>,
    created_at: String,
}

#[allow(dead_code)]
struct UnifiedWorkflowRow {
    id: String,
    name: String,
    description: Option<String>,
    category: Option<String>,
    tags: Option<String>,
    setup_steps: Option<String>,
    verification_steps: Option<String>,
    agentic_steps: Option<String>,
    completion_steps: Option<String>,
    max_iterations: Option<i32>,
    provider: Option<String>,
    model: Option<String>,
    skip_ai_summary: bool,
    timeout_seconds: Option<i32>,
    prompt_template: Option<String>,
    context_ids: Option<String>,
    disabled_context_ids: Option<String>,
    auto_include_contexts: Option<bool>,
    log_watch_enabled: Option<bool>,
    log_source_selection: Option<String>,
    health_check_enabled: Option<bool>,
    health_check_urls: Option<String>,
    preflight_check_enabled: Option<bool>,
    enable_sweep: Option<bool>,
    max_sweep_iterations: Option<i32>,
    stages: Option<String>,
    stop_on_failure: Option<bool>,
    constraint_overrides: Option<String>,
    approval_gate: Option<bool>,
    reflection_mode: Option<bool>,
    completion_prompts_first: bool,
    model_overrides: Option<String>,
    example_status: Option<String>,
    sync_pending: Option<bool>,
    is_favorite: Option<bool>,
    dependency_graph: Option<String>,
    cost_annotations: Option<String>,
    quality_report: Option<String>,
    acceptance_criteria: Option<String>,
    ai_reviewed: Option<bool>,
    workflow_architecture: Option<String>,
    source_file_path: Option<String>,
    source_content_hash: Option<String>,
    rollback_policy: Option<String>,
    strict_cwd: Option<bool>,
    tool_tags: Option<String>,
    enforce_token_budget: Option<bool>,
    flow_control_json: Option<String>,
    phase_timeouts_json: Option<String>,
    created_at: String,
    updated_at: String,
}

struct TaskRunRow {
    id: String,
    task_name: String,
    prompt: Option<String>,
    task_type: String,
    status: String,
    sessions_count: i32,
    max_sessions: Option<i32>,
    auto_continue: bool,
    error_message: Option<String>,
    execution_steps_json: Option<String>,
    log_sources_json: Option<String>,
    config_id: Option<String>,
    workflow_name: Option<String>,
    workflow_id: Option<String>,
    summary: Option<String>,
    ai_summary: Option<String>,
    goal_achieved: Option<bool>,
    remaining_work: Option<String>,
    summary_generated_at: Option<String>,
    runtime_context_json: Option<String>,
    transition_history_json: Option<String>,
    parent_task_run_id: Option<String>,
    root_task_run_id: Option<String>,
    depth: Option<i32>,
    bridge_id: Option<String>,
    workflow_type: Option<String>,
    result_data: Option<String>,
    workspace_id: Option<String>,
    triggered_by: Option<String>,
    is_reflection: Option<i32>,
    reflection_source_task_run_id: Option<String>,
    is_follow_up: Option<i32>,
    follow_up_source_task_run_id: Option<String>,
    runner_port: Option<i32>,
    is_fixer: Option<i32>,
    fixer_source_task_run_id: Option<String>,
    is_meta_optimizer: Option<i32>,
    iteration_history: Option<String>,
    pipeline_checkpoint: Option<String>,
    iteration_diffs: Option<String>,
    iteration_commits: Option<String>,
    verification_passed: Option<i32>,
    total_input_tokens: Option<i32>,
    total_output_tokens: Option<i32>,
    total_cost_cents: Option<i32>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

struct FindingRow {
    id: String,
    task_run_id: String,
    category: String,
    severity: String,
    signature_hash: Option<String>,
    title: String,
    description: String,
    file_path: Option<String>,
    line_number: Option<i32>,
    column_number: Option<i32>,
    code_snippet: Option<String>,
    status: String,
    action_type: String,
    resolution: Option<String>,
    detected_in_session: i32,
    resolved_in_session: Option<i32>,
    needs_input: Option<bool>,
    question: Option<String>,
    input_options: Option<String>,
    user_response: Option<String>,
    reflection_fix_id: Option<String>,
    detected_at: String,
    resolved_at: Option<String>,
    updated_at: String,
}
