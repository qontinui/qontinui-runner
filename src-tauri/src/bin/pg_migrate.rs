//! Standalone SQLite → PostgreSQL data migration tool.
//!
//! Copies all data from the runner's SQLite database to PostgreSQL.
//! Uses dynamic table/column introspection — no per-table boilerplate needed.
//!
//! Run:
//!   cargo run --bin pg_migrate
//!
//! Environment:
//!   RUNNER_DATABASE_URL — PG connection string (libpq format or URL)
//!   SQLITE_PATH        — Override SQLite path (default: Tauri app data dir)

use std::collections::{HashMap, HashSet};

use chrono::{NaiveDateTime, TimeZone, Utc};
use rusqlite::{types::ValueRef, Connection};
use tokio_postgres::{types::ToSql, NoTls};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tables to skip entirely (FTS virtual tables, internal bookkeeping).
const SKIP_TABLES: &[&str] = &[
    "schema_version",
    "sqlite_sequence",
    // FTS virtual tables and their shadow tables
    "error_events_fts",
    "error_events_fts_data",
    "error_events_fts_idx",
    "error_events_fts_content",
    "error_events_fts_docsize",
    "error_events_fts_config",
];

/// Prefixes that indicate FTS shadow tables.
const SKIP_PREFIXES: &[&str] = &["error_events_fts_"];

/// Tables with self-referencing FKs that need special ordering.
/// Listed in the order they should be migrated.
const ORDERED_TABLES: &[&str] = &[
    // Tier 1: No FK dependencies
    "settings",
    "configs",
    "shell_commands",
    "mcp_servers",
    "saved_api_requests",
    "checks",
    "check_groups",
    "user_skills",
    "learning_patterns",
    "learning_outcomes",
    "unified_workflows",
    "stall_events",
    "state_machine_configs",
    "process_sessions",
    "scheduled_tasks",
    // Tier 2: task_runs (self-referencing parent/root FK)
    "task_runs",
    // Tier 2b: Depends on task_runs
    "phase_token_usage",
    "task_run_events",
    "task_run_findings",
    "task_run_screenshots",
    "workflow_step_checkpoints",
    "workflow_execution_state",
    "approval_gates",
    "ui_bridge_elements",
    "ui_bridge_events",
    // Tier 3: Depends on tier 1 + tier 2
    "check_group_members",
    "step_progress_markers",
    // Tier 4: Depends on tier 1 parent tables
    "process_session_output",
    "sm_capture_screenshots",
    "sm_element_thumbnails",
    "scheduler_history",
];

/// SQLite columns that store booleans as INTEGER 0/1 but PG expects BOOLEAN.
/// Format: (table, column).
const BOOL_COLUMNS: &[(&str, &str)] = &[
    ("shell_commands", "fail_on_error"),
    ("shell_commands", "enabled"),
    ("mcp_servers", "enabled"),
    ("mcp_servers", "auto_start"),
    ("saved_api_requests", "follow_redirects"),
    ("checks", "auto_fix"),
    ("checks", "fail_on_warning"),
    ("checks", "is_critical"),
    ("checks", "enabled"),
    ("checks", "ai_generated"),
    ("check_groups", "enabled"),
    ("check_groups", "run_in_parallel"),
    ("check_groups", "stop_on_failure"),
    ("task_runs", "auto_continue"),
    ("task_runs", "is_reflection"),
    ("task_runs", "is_follow_up"),
    ("task_runs", "is_fixer"),
    ("task_runs", "is_meta_optimizer"),
    ("task_runs", "verification_passed"),
    ("task_runs", "goal_achieved"),
    ("unified_workflows", "skip_ai_summary"),
    ("unified_workflows", "auto_include_contexts"),
    ("unified_workflows", "log_watch_enabled"),
    ("unified_workflows", "health_check_enabled"),
    ("unified_workflows", "preflight_check_enabled"),
    ("unified_workflows", "enable_sweep"),
    ("unified_workflows", "stop_on_failure"),
    ("unified_workflows", "approval_gate"),
    ("unified_workflows", "reflection_mode"),
    ("unified_workflows", "completion_prompts_first"),
    ("unified_workflows", "sync_pending"),
    ("unified_workflows", "is_favorite"),
    ("unified_workflows", "ai_reviewed"),
    ("unified_workflows", "strict_cwd"),
    ("unified_workflows", "enforce_token_budget"),
    ("task_run_findings", "needs_input"),
    ("learning_outcomes", "has_ui_bridge"),
    ("ui_bridge_elements", "visible"),
    ("ui_bridge_elements", "enabled"),
    ("ui_bridge_elements", "focused"),
    ("ui_bridge_events", "success"),
];

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MigrationStats {
    tables_migrated: u32,
    tables_skipped: u32,
    rows_migrated: u64,
    errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    tracing_subscriber::fmt::init();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    rt.block_on(async {
        if let Err(e) = run().await {
            eprintln!("FATAL: {}", e);
            std::process::exit(1);
        }
    });
}

async fn run() -> Result<(), String> {
    // ---- SQLite ----
    let sqlite_path = resolve_sqlite_path()?;
    println!("SQLite: {}", sqlite_path.display());

    let sqlite = Connection::open_with_flags(
        &sqlite_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("Failed to open SQLite: {}", e))?;

    // ---- PostgreSQL ----
    let pg_url = std::env::var("RUNNER_DATABASE_URL").unwrap_or_else(|_| {
        "host=localhost port=5432 user=qontinui_user password=qontinui_dev_password dbname=qontinui_db"
            .to_string()
    });
    println!("Connecting to PostgreSQL...");
    let (pg, connection) = tokio_postgres::connect(&pg_url, NoTls)
        .await
        .map_err(|e| format!("PG connect: {}", e))?;

    // Spawn the connection future so it runs in the background
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("PG connection error: {}", e);
        }
    });

    println!("PostgreSQL connected");

    // Set search_path to include runner schema (matches app behaviour)
    pg.execute("SET search_path TO runner, public", &[])
        .await
        .map_err(|e| format!("PG set search_path: {}", e))?;

    // ---- Quick-check: already migrated? ----
    if let Ok(row) = pg.query_one("SELECT COUNT(*) FROM task_runs", &[]).await {
        let count: i64 = row.get(0);
        if count > 0 {
            println!(
                "PG already has {} task_runs — data appears migrated. Proceeding anyway (ON CONFLICT DO NOTHING).",
                count
            );
        }
    }

    // ---- Discover SQLite tables ----
    let sqlite_tables = list_sqlite_tables(&sqlite)?;
    println!("SQLite tables found: {}", sqlite_tables.len());

    // ---- Discover PG tables ----
    let pg_tables = list_pg_tables(&pg).await?;
    println!("PG tables found: {}", pg_tables.len());

    // Build bool-column lookup
    let bool_set: HashSet<(&str, &str)> = BOOL_COLUMNS.iter().copied().collect();

    // ---- Migrate in dependency order ----
    let mut stats = MigrationStats::default();
    let mut migrated_set: HashSet<String> = HashSet::new();

    // First: ordered tables
    for &table in ORDERED_TABLES {
        if !sqlite_tables.iter().any(|t| t == table) {
            continue;
        }
        if !pg_tables.iter().any(|t| t == table) {
            println!("  SKIP {} (no PG table)", table);
            stats.tables_skipped += 1;
            continue;
        }
        migrate_table(&sqlite, &pg, table, &bool_set, &mut stats).await;
        migrated_set.insert(table.to_string());

        // Back-fill unified_workflows.generated_by_task_run_id after task_runs
        if table == "task_runs" {
            backfill_unified_workflow_fk(&sqlite, &pg).await;
        }
    }

    // Then: any remaining tables not in the ordered list
    let mut remaining: Vec<&str> = sqlite_tables
        .iter()
        .filter(|t| !migrated_set.contains(t.as_str()) && !should_skip(t))
        .map(|s| s.as_str())
        .collect();
    remaining.sort();

    for table in remaining {
        if !pg_tables.contains(table) {
            println!("  SKIP {} (no PG table)", table);
            stats.tables_skipped += 1;
            continue;
        }
        migrate_table(&sqlite, &pg, table, &bool_set, &mut stats).await;
    }

    // ---- Report ----
    println!("\n========================================");
    println!("  Migration Complete");
    println!("========================================");
    println!("  Tables migrated : {}", stats.tables_migrated);
    println!("  Tables skipped  : {}", stats.tables_skipped);
    println!("  Rows migrated   : {}", stats.rows_migrated);
    if !stats.errors.is_empty() {
        println!("  Errors          : {}", stats.errors.len());
        for (i, e) in stats.errors.iter().enumerate().take(20) {
            println!("    [{}] {}", i + 1, e);
        }
        if stats.errors.len() > 20 {
            println!("    ... and {} more", stats.errors.len() - 20);
        }
    }
    println!("========================================");

    Ok(())
}

// ---------------------------------------------------------------------------
// SQLite path resolution
// ---------------------------------------------------------------------------

fn resolve_sqlite_path() -> Result<std::path::PathBuf, String> {
    // 1. Explicit env override
    if let Ok(p) = std::env::var("SQLITE_PATH") {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
        return Err(format!("SQLITE_PATH set but file not found: {}", path.display()));
    }

    // 2. Tauri app data directory: {config_dir}/com.qontinui.runner/runner.db
    if let Some(config) = dirs::config_dir() {
        let path = config.join("com.qontinui.runner").join("runner.db");
        if path.exists() {
            return Ok(path);
        }
        // Also try data_dir (Linux uses ~/.local/share)
        if let Some(data) = dirs::data_dir() {
            let path2 = data.join("com.qontinui.runner").join("runner.db");
            if path2.exists() {
                return Ok(path2);
            }
        }
        return Err(format!(
            "SQLite not found at expected Tauri path: {}. Set SQLITE_PATH env to override.",
            path.display()
        ));
    }

    Err("Cannot determine config directory. Set SQLITE_PATH env.".to_string())
}

// ---------------------------------------------------------------------------
// Table discovery
// ---------------------------------------------------------------------------

fn list_sqlite_tables(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .map_err(|e| format!("sqlite list tables: {}", e))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("sqlite list tables: {}", e))?;
    let mut tables = Vec::new();
    for r in rows {
        let name = r.map_err(|e| format!("sqlite table row: {}", e))?;
        if !should_skip(&name) {
            tables.push(name);
        }
    }
    Ok(tables)
}

async fn list_pg_tables(pg: &tokio_postgres::Client) -> Result<HashSet<String>, String> {
    let rows = pg
        .query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema IN ('runner', 'public') AND table_type = 'BASE TABLE'",
            &[],
        )
        .await
        .map_err(|e| format!("PG list tables: {}", e))?;
    Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
}

fn should_skip(table: &str) -> bool {
    if SKIP_TABLES.contains(&table) {
        return true;
    }
    for prefix in SKIP_PREFIXES {
        if table.starts_with(prefix) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// PG column type lookup
// ---------------------------------------------------------------------------

/// Info about a PG column: data_type from information_schema + udt_name for USER-DEFINED.
#[derive(Debug, Clone)]
struct PgColInfo {
    data_type: String,
    udt_name: String,
}

async fn pg_column_info(
    pg: &tokio_postgres::Client,
    table: &str,
) -> Result<HashMap<String, PgColInfo>, String> {
    // Prefer runner schema over public when the table exists in both.
    // ORDER BY table_schema DESC puts 'runner' after 'public', so runner wins
    // when inserted into the HashMap (last write wins).
    let rows = pg
        .query(
            "SELECT column_name, data_type, udt_name FROM information_schema.columns \
             WHERE table_name = $1 AND table_schema IN ('runner', 'public') \
             ORDER BY table_schema ASC, ordinal_position",
            &[&table],
        )
        .await
        .map_err(|e| format!("{}: pg column types: {}", table, e))?;
    // Build map: public columns inserted first, runner columns overwrite them.
    let mut map = HashMap::new();
    for r in &rows {
        map.insert(
            r.get::<_, String>(0),
            PgColInfo {
                data_type: r.get::<_, String>(1),
                udt_name: r.get::<_, String>(2),
            },
        );
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Generic table migration
// ---------------------------------------------------------------------------

/// Classify a PG column into one of the coercion kinds we support.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ColKind {
    Text,
    Bool,
    Int32,
    Int64,
    Float64,
    Timestamp,
    Blob,
    /// UUID, JSONB, USER-DEFINED enums, etc. — send as Text with SQL cast.
    CastText,
}

fn classify_column(table: &str, col: &str, info: &PgColInfo, bool_cols: &HashSet<(&str, &str)>) -> ColKind {
    // Explicit bool override from BOOL_COLUMNS list
    if bool_cols.contains(&(table, col)) {
        return ColKind::Bool;
    }
    let dt = info.data_type.as_str();
    match dt {
        "boolean" => ColKind::Bool,
        "smallint" | "integer" => ColKind::Int32,
        "bigint" => ColKind::Int64,
        "double precision" | "real" | "numeric" => ColKind::Float64,
        "bytea" => ColKind::Blob,
        _ if dt.contains("timestamp") => ColKind::Timestamp,
        // text, character varying — plain text, no cast needed
        "text" | "character varying" | "character" => ColKind::Text,
        // uuid, jsonb, json, USER-DEFINED (enums), ARRAY, etc. — need SQL cast
        _ => ColKind::CastText,
    }
}

/// Build the SQL placeholder for a column.
/// For CastText columns, generates `$N::text::udt_name` — the double cast tells PG
/// to accept the parameter as TEXT (matching the Rust String wire type) and then
/// cast to the target type server-side.
fn build_placeholder(idx: usize, kind: ColKind, info: &PgColInfo) -> String {
    let n = idx + 1;
    match kind {
        ColKind::CastText => {
            // Double cast: text (wire) -> target type (server-side)
            format!("${}::text::{}", n, info.udt_name)
        }
        _ => format!("${}", n),
    }
}

async fn migrate_table(
    sqlite: &Connection,
    pg: &tokio_postgres::Client,
    table: &str,
    bool_cols: &HashSet<(&str, &str)>,
    stats: &mut MigrationStats,
) {
    // Get SQLite column names
    let sqlite_cols = match get_sqlite_columns(sqlite, table) {
        Ok(c) => c,
        Err(e) => {
            stats.errors.push(format!("{}: {}", table, e));
            return;
        }
    };

    if sqlite_cols.is_empty() {
        stats.tables_skipped += 1;
        return;
    }

    // Get PG column info for type coercion
    let pg_col_info = match pg_column_info(pg, table).await {
        Ok(m) => m,
        Err(e) => {
            stats.errors.push(e);
            return;
        }
    };

    // Only migrate columns that exist in both SQLite and PG.
    // Exclude columns that have forward FK references handled by backfill.
    let skip_cols: &[(&str, &str)] = &[
        // unified_workflows.generated_by_task_run_id → task_runs (backfilled after task_runs)
        ("unified_workflows", "generated_by_task_run_id"),
    ];
    let columns: Vec<String> = sqlite_cols
        .into_iter()
        .filter(|c| {
            pg_col_info.contains_key(c)
                && !skip_cols.iter().any(|(t, col)| *t == table && *col == c.as_str())
        })
        .collect();

    if columns.is_empty() {
        println!("  SKIP {} (no overlapping columns)", table);
        stats.tables_skipped += 1;
        return;
    }

    // Classify each column
    let col_kinds: Vec<ColKind> = columns
        .iter()
        .map(|col| {
            let info = pg_col_info.get(col).unwrap();
            classify_column(table, col, info, bool_cols)
        })
        .collect();

    // For task_runs: order by depth ASC, created_at ASC (parents before children)
    let order_clause = if table == "task_runs" {
        " ORDER BY depth ASC, created_at ASC"
    } else {
        ""
    };

    let select_sql = format!(
        "SELECT {} FROM {}{}",
        columns.join(", "),
        table,
        order_clause,
    );

    // Read all rows from SQLite
    let rows: Vec<Vec<SqliteValue>> = match read_sqlite_rows(sqlite, &select_sql, columns.len()) {
        Ok(r) => r,
        Err(e) => {
            // Table may not exist in older schemas
            if e.contains("no such table") {
                stats.tables_skipped += 1;
                return;
            }
            stats.errors.push(format!("{}: {}", table, e));
            return;
        }
    };

    if rows.is_empty() {
        println!("  {} — 0 rows (empty)", table);
        stats.tables_skipped += 1;
        return;
    }

    // Build INSERT statement with typed parameter placeholders
    let placeholders: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let info = pg_col_info.get(col).unwrap();
            build_placeholder(i, col_kinds[i], info)
        })
        .collect();

    let insert_sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT DO NOTHING",
        table,
        columns.join(", "),
        placeholders.join(", "),
    );

    // Insert rows
    let mut count = 0u64;
    let mut err_count = 0u64;
    let total = rows.len();

    for (row_idx, row) in rows.iter().enumerate() {
        // Convert SqliteValue to PG-compatible params
        let params: Vec<PgParam> = row
            .iter()
            .enumerate()
            .map(|(i, val)| to_pg_param(val, col_kinds[i]))
            .collect();

        let param_refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();

        match pg.execute(&insert_sql as &str, &param_refs).await {
            Ok(_) => count += 1,
            Err(e) => {
                // On first error per table, print detailed debug info
                if err_count == 0 {
                    eprintln!("  DEBUG {}: ERROR = {:?}", table, e);
                    eprintln!("  DEBUG {}: SQL = {}", table, insert_sql);
                    for (i, (col, param)) in columns.iter().zip(params.iter()).enumerate() {
                        let info = pg_col_info.get(col).unwrap();
                        let val_desc = match param {
                            PgParam::Null => "NULL".to_string(),
                            PgParam::Text(s) => format!("Text(len={})", s.len()),
                            PgParam::Timestamp(Some(dt)) => format!("Timestamp({dt})"),
                            PgParam::Timestamp(None) => "Timestamp(NULL)".to_string(),
                            PgParam::Int64(v) => format!("Int64({v})"),
                            PgParam::Int32(v) => format!("Int32({v})"),
                            PgParam::Bool(v) => format!("Bool({v})"),
                            PgParam::Float64(v) => format!("Float64({v})"),
                            PgParam::Blob(v) => format!("Blob(len={})", v.len()),
                            PgParam::OptBool(v) => format!("OptBool({v:?})"),
                            PgParam::OptInt32(v) => format!("OptInt32({v:?})"),
                            PgParam::OptInt64(v) => format!("OptInt64({v:?})"),
                            PgParam::OptFloat64(v) => format!("OptFloat64({v:?})"),
                            PgParam::OptText(v) => format!("OptText({:?})", v.as_ref().map(|s| format!("len={}", s.len()))),
                            PgParam::OptBlob(v) => format!("OptBlob({:?})", v.as_ref().map(|b| format!("len={}", b.len()))),
                        };
                        eprintln!("    ${}: {} (pg={}:{}, kind={:?}) = {}", i+1, col, info.data_type, info.udt_name, col_kinds[i], val_desc);
                    }
                }
                err_count += 1;
                if err_count <= 3 {
                    stats
                        .errors
                        .push(format!("{}: row {}: {}", table, row_idx, e));
                } else if err_count == 4 {
                    stats
                        .errors
                        .push(format!("{}: ... suppressing further errors", table));
                }
            }
        }

        // Progress reporting for large tables
        if total > 1000 && (row_idx + 1) % 1000 == 0 {
            print!("\r  {} — {}/{} rows...", table, row_idx + 1, total);
        }
    }

    if total > 1000 {
        print!("\r");
    }

    if count > 0 {
        println!("  {} — {} rows migrated (of {})", table, count, total);
        stats.tables_migrated += 1;
        stats.rows_migrated += count;
    } else if err_count > 0 {
        println!(
            "  {} — 0 rows migrated, {} errors (of {})",
            table, err_count, total
        );
    } else {
        println!("  {} — 0 rows (all skipped/conflict)", table);
        stats.tables_skipped += 1;
    }
}

// ---------------------------------------------------------------------------
// unified_workflows FK backfill
// ---------------------------------------------------------------------------

async fn backfill_unified_workflow_fk(sqlite: &Connection, pg: &tokio_postgres::Client) {
    let rows: Vec<(String, String)> = match (|| -> Result<_, String> {
        let mut stmt = sqlite
            .prepare(
                "SELECT id, generated_by_task_run_id FROM unified_workflows \
                 WHERE generated_by_task_run_id IS NOT NULL",
            )
            .map_err(|e| format!("backfill read: {}", e))?;
        let mapped = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("backfill query: {}", e))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("backfill collect: {}", e))
    })() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  backfill_unified_workflow_fk: {}", e);
            return;
        }
    };

    let mut updated = 0u64;
    for (wf_id, tr_id) in &rows {
        if let Ok(n) = pg
            .execute(
                "UPDATE unified_workflows SET generated_by_task_run_id = $1 WHERE id = $2",
                &[tr_id, wf_id],
            )
            .await
        {
            updated += n;
        }
    }
    if updated > 0 {
        println!(
            "  unified_workflows — backfilled {} generated_by_task_run_id FKs",
            updated
        );
    }
}

// ---------------------------------------------------------------------------
// SQLite helpers
// ---------------------------------------------------------------------------

fn get_sqlite_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .map_err(|e| format!("pragma table_info: {}", e))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("pragma table_info: {}", e))?;
    cols.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("pragma table_info collect: {}", e))
}

/// Intermediate representation for a SQLite cell value.
#[derive(Debug, Clone)]
enum SqliteValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

fn read_sqlite_rows(
    conn: &Connection,
    sql: &str,
    ncols: usize,
) -> Result<Vec<Vec<SqliteValue>>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| format!("prepare: {}", e))?;
    let mut sqlite_rows = stmt.query([]).map_err(|e| format!("query: {}", e))?;
    let mut out = Vec::new();
    while let Some(row) = sqlite_rows.next().map_err(|e| format!("next: {}", e))? {
        let mut vals = Vec::with_capacity(ncols);
        for i in 0..ncols {
            let val = match row.get_ref(i).map_err(|e| format!("col {}: {}", i, e))? {
                ValueRef::Null => SqliteValue::Null,
                ValueRef::Integer(v) => SqliteValue::Integer(v),
                ValueRef::Real(v) => SqliteValue::Real(v),
                ValueRef::Text(v) => {
                    SqliteValue::Text(String::from_utf8_lossy(v).into_owned())
                }
                ValueRef::Blob(v) => SqliteValue::Blob(v.to_vec()),
            };
            vals.push(val);
        }
        out.push(vals);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// PG parameter wrapper
// ---------------------------------------------------------------------------

/// A dynamically-typed PG parameter. We need to own the value and implement
/// `ToSql + Sync` so it can be passed as `&[&(dyn ToSql + Sync)]`.
enum PgParam {
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Text(String),
    Blob(Vec<u8>),
    Null,
    OptBool(Option<bool>),
    OptInt32(Option<i32>),
    OptInt64(Option<i64>),
    OptFloat64(Option<f64>),
    OptText(Option<String>),
    OptBlob(Option<Vec<u8>>),
    /// Timestamp parsed from SQLite text into chrono DateTime for PG TIMESTAMPTZ
    Timestamp(Option<chrono::DateTime<Utc>>),
}

impl PgParam {
    fn as_ref(&self) -> &(dyn ToSql + Sync) {
        match self {
            PgParam::Null => &None::<String>,
            PgParam::Bool(v) => v,
            PgParam::Int32(v) => v,
            PgParam::Int64(v) => v,
            PgParam::Float64(v) => v,
            PgParam::Text(v) => v,
            PgParam::Blob(v) => v,
            PgParam::OptBool(v) => v,
            PgParam::OptInt32(v) => v,
            PgParam::OptInt64(v) => v,
            PgParam::OptFloat64(v) => v,
            PgParam::OptText(v) => v,
            PgParam::OptBlob(v) => v,
            PgParam::Timestamp(v) => v,
        }
    }
}

/// Parse a SQLite timestamp string into a chrono DateTime<Utc>.
/// Handles common formats: ISO-8601, with/without timezone, with/without fractional seconds.
fn parse_timestamp(s: &str) -> Option<chrono::DateTime<Utc>> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Try parsing as DateTime with timezone info (e.g. "2024-01-15T10:30:00Z",
    // "2024-01-15 10:30:00+00:00", "2024-01-15T10:30:00.123Z")
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try common SQLite formats (no timezone — assume UTC)
    let naive_formats = [
        "%Y-%m-%d %H:%M:%S%.f", // "2024-01-15 10:30:00.123"
        "%Y-%m-%d %H:%M:%S",    // "2024-01-15 10:30:00"
        "%Y-%m-%dT%H:%M:%S%.f", // "2024-01-15T10:30:00.123"
        "%Y-%m-%dT%H:%M:%S",    // "2024-01-15T10:30:00"
        "%Y-%m-%d",              // "2024-01-15"
    ];

    for fmt in &naive_formats {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(Utc.from_utc_datetime(&ndt));
        }
    }

    // Last resort: try parsing with timezone offset like "+00:00" suffix
    // e.g. "2024-01-15 10:30:00+00:00"
    if let Ok(dt) = chrono::DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f%:z") {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = chrono::DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%:z") {
        return Some(dt.with_timezone(&Utc));
    }

    None
}

fn to_pg_param(val: &SqliteValue, kind: ColKind) -> PgParam {
    match val {
        SqliteValue::Null => match kind {
            ColKind::Timestamp => PgParam::Timestamp(None),
            ColKind::Bool => PgParam::OptBool(None),
            ColKind::Int32 => PgParam::OptInt32(None),
            ColKind::Int64 => PgParam::OptInt64(None),
            ColKind::Float64 => PgParam::OptFloat64(None),
            ColKind::Blob => PgParam::OptBlob(None),
            ColKind::Text | ColKind::CastText => PgParam::OptText(None),
        },
        SqliteValue::Integer(v) => match kind {
            ColKind::Bool => PgParam::Bool(*v != 0),
            ColKind::Int32 => PgParam::Int32(*v as i32),
            ColKind::Float64 => PgParam::Float64(*v as f64),
            ColKind::Timestamp => {
                let dt = Utc.timestamp_opt(*v, 0).single();
                PgParam::Timestamp(dt)
            }
            ColKind::CastText => PgParam::Text(v.to_string()),
            // Int64, Text, Blob — default to Int64
            _ => PgParam::Int64(*v),
        },
        SqliteValue::Real(v) => match kind {
            ColKind::Timestamp => {
                let secs = *v as i64;
                let nsecs = ((*v - secs as f64) * 1_000_000_000.0) as u32;
                let dt = Utc.timestamp_opt(secs, nsecs).single();
                PgParam::Timestamp(dt)
            }
            ColKind::Int32 => PgParam::Int32(*v as i32),
            ColKind::Int64 => PgParam::Int64(*v as i64),
            ColKind::CastText => PgParam::Text(v.to_string()),
            _ => PgParam::Float64(*v),
        },
        SqliteValue::Text(v) => match kind {
            ColKind::Timestamp => PgParam::Timestamp(parse_timestamp(v)),
            ColKind::Bool => {
                let b = v == "1" || v.eq_ignore_ascii_case("true");
                PgParam::Bool(b)
            }
            ColKind::Int32 => PgParam::Int32(v.parse().unwrap_or(0)),
            ColKind::Int64 => PgParam::Int64(v.parse().unwrap_or(0)),
            ColKind::Float64 => PgParam::Float64(v.parse().unwrap_or(0.0)),
            // Text, CastText, Blob — send as text
            _ => PgParam::Text(v.clone()),
        },
        SqliteValue::Blob(v) => match kind {
            ColKind::Blob => PgParam::Blob(v.clone()),
            // If PG expects text but SQLite has blob, try to convert
            _ => match String::from_utf8(v.clone()) {
                Ok(s) => PgParam::Text(s),
                Err(_) => PgParam::Blob(v.clone()),
            },
        },
    }
}
