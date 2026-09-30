//! Boot-time move of the six Atlas-owned tables from `project` into
//! `atlas_managed` (plan `2026-05-14-atlas-wave-6-triage`, Phase 3a).
//!
//! Atlas owns the `atlas_managed` schema wholly, and every runner query names
//! these tables as `atlas_managed.<t>`. No alembic revision moves them: a
//! database built by alembic alone still carries the four legacy
//! `project.regression_*` tables from frozen revision `f9d3e8a4c1b6`, a
//! user's embedded cluster provisioned from an older `schema.pg.sql.generated`
//! holds them in `project`, and a shared database may hold copies in both
//! schemas. [`migrate_atlas_managed_tables`] reconciles that on every boot, in
//! FK order, per table:
//!
//! * only `project.<t>` exists → `ALTER TABLE … SET SCHEMA atlas_managed`.
//!   Rows, indexes and constraints move atomically. `SET SCHEMA` leaves an FK
//!   pointing at its original parent, so every FK of the moved table whose
//!   parent is still a `project` table with a same-named `atlas_managed` twin
//!   is re-created against the twin (`NOT VALID`, then `VALIDATE`). If any
//!   step fails the move is rolled back and the table stays where it was
//!   ([`TableMoveOutcome::NotMoved`]).
//! * both exist → MERGE: copy every legacy row with `ON CONFLICT DO NOTHING`,
//!   then check the copy. The legacy table is dropped only when (a) every
//!   legacy primary key is present in `atlas_managed.<t>`, (b) for every legacy
//!   row, each column the two copies share holds the same value in the target
//!   (compared as text), and (c) the drop needs no `CASCADE`. When (a) or (b)
//!   fails the copy is rolled back — nothing is left half-merged, so a row
//!   deleted from the target is never resurrected by a later boot — and both
//!   tables stay ([`TableMoveOutcome::KeptBoth`]). When only (c) fails the
//!   whole pass is re-run with that table excluded from merging, for the same
//!   reason. Rows that did not make it across are never dropped.
//! * only `atlas_managed.<t>` exists, or neither → nothing to do.
//!
//! Why the runner may DROP a legacy `project.<t>` table at all, when alembic
//! is otherwise the sole DDL author of `project`: it happens only after a
//! merge verified that every legacy row is present and identical in
//! `atlas_managed.<t>`, so no data is lost; the alembic-created copies (frozen
//! revision `f9d3e8a4c1b6`) have no reader anywhere — nothing in qontinui-web or
//! coord queries these tables, and every runner query now names
//! `atlas_managed.<t>`; and runners never point at production RDS, so the
//! databases this runs against are embedded clusters and dev/CI databases.
//! alembic never re-creates a dropped copy, since `f9d3e8a4c1b6` is history.
//!
//! A steady-state boot costs one catalog query: when no `project.<t>` of the
//! six exists, the pass returns before opening a transaction, taking the
//! advisory lock or issuing `CREATE SCHEMA` (so it needs no CREATE privilege).
//!
//! The pass runs in one transaction under a transaction-scoped advisory lock,
//! so two runners booting against one cluster (a primary and a temp runner
//! share the embedded PG) serialize instead of racing. `lock_timeout` and
//! `statement_timeout` bound it so a contended table cannot stall boot. A
//! pass that fails outright rolls back and is REPORTED, never raised: boot
//! continues, and any query against a table left behind fails loudly on its
//! own because `atlas_managed` is not on the search_path.

use std::time::Duration;

use tokio_postgres::{Client, GenericClient, Transaction};
use tracing::{debug, error, info, warn};

/// The schema Atlas owns and every runner query addresses.
pub const ATLAS_MANAGED_SCHEMA: &str = "atlas_managed";

/// Where the six tables lived before the move.
pub const LEGACY_SCHEMA: &str = "project";

/// The six Atlas-owned tables, parents before children. Every FK among them
/// is intra-set (`regression_runs` → `regression_suites`;
/// `regression_diagnoses` / `regression_assertion_executions` →
/// `regression_runs`), and nothing outside the set references them. Moves and
/// merges run in this order and legacy drops in its reverse.
pub const ATLAS_MANAGED_TABLES: [&str; 6] = [
    "regression_suites",
    "regression_runs",
    "regression_diagnoses",
    "regression_assertion_executions",
    "spec_proposals",
    "proposal_events",
];

/// Columns added to a table after it first shipped, with the value a legacy
/// row gets when its copy predates the column. `proposal_events.app_id` is the
/// spec-multi-app Stream E.1 column; `'qontinui-runner'` is the value the
/// app_id backfill in `verify_and_provision` gives every pre-multi-app row.
const LEGACY_COLUMN_FILLS: &[(&str, &str, &str)] =
    &[("proposal_events", "app_id", "'qontinui-runner'")];

/// `pg_advisory_xact_lock` key for the move ("atlasmv" in ASCII).
pub(crate) const MOVE_LOCK_KEY: i64 = 0x0061_746c_6173_6d76;

/// How long the pass waits for any single lock (the advisory lock, or a table
/// lock for `SET SCHEMA` / `DROP`) before giving up for this boot.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on any single statement of the pass (the merge copy of a large
/// table is the long one).
pub(crate) const STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);

/// What happened to one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableMoveOutcome {
    /// Neither `project.<t>` nor `atlas_managed.<t>` exists (e.g. an embedded
    /// cluster built before `spec_proposals` was in the bundled schema).
    Absent,
    /// Only `atlas_managed.<t>` exists — the steady state.
    AlreadyInTarget,
    /// `project.<t>` was moved with `SET SCHEMA`; `repointed_fks` FKs were
    /// re-created against their `atlas_managed` parent.
    Moved { repointed_fks: u32 },
    /// Only `project.<t>` exists and it could NOT be moved (a lock timeout, a
    /// name clash, or an FK that does not validate against the
    /// `atlas_managed` parent). It was left in `project` untouched.
    NotMoved { reason: String },
    /// Both existed; every legacy row was verified present and identical in
    /// the target and the legacy table was dropped.
    Merged { legacy_rows: i64, inserted: u64 },
    /// Both existed and the legacy copy could NOT be retired; nothing was
    /// copied and both tables were left in place. `missing_rows` legacy rows
    /// have no PK match in the target, `differing_rows` match a target row by
    /// PK but differ in a shared column (`-1` when not counted).
    KeptBoth {
        legacy_rows: i64,
        missing_rows: i64,
        differing_rows: i64,
        reason: String,
    },
}

/// The result of one [`migrate_atlas_managed_tables`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AtlasManagedMoveReport {
    /// Per-table outcomes in FK order. Empty when `pass_error` is set.
    pub tables: Vec<(&'static str, TableMoveOutcome)>,
    /// The pass could not run at all (lock timeout on the advisory lock,
    /// schema creation, a catalog read, COMMIT) and was rolled back.
    pub pass_error: Option<String>,
}

impl AtlasManagedMoveReport {
    /// Tables left in `project` — kept beside a target copy, or not moved.
    pub fn left_behind(&self) -> impl Iterator<Item = &(&'static str, TableMoveOutcome)> {
        self.tables.iter().filter(|(_, o)| {
            matches!(
                o,
                TableMoveOutcome::KeptBoth { .. } | TableMoveOutcome::NotMoved { .. }
            )
        })
    }

    /// Whether this pass did or attempted anything beyond confirming the
    /// steady state.
    pub fn changed_anything(&self) -> bool {
        self.pass_error.is_some()
            || self.tables.iter().any(|(_, o)| {
                !matches!(
                    o,
                    TableMoveOutcome::Absent | TableMoveOutcome::AlreadyInTarget
                )
            })
    }

    /// The outcome recorded for `table`.
    pub fn outcome(&self, table: &str) -> Option<&TableMoveOutcome> {
        self.tables
            .iter()
            .find(|(t, _)| *t == table)
            .map(|(_, o)| o)
    }
}

/// Move (or merge) any `project.<t>` copy of the six Atlas-owned tables into
/// `atlas_managed`, and log the outcome. Idempotent: on a converged database
/// it creates nothing and reports `AlreadyInTarget` / `Absent` for every table.
///
/// Never fails: a pass that cannot run is rolled back and returned with
/// [`AtlasManagedMoveReport::pass_error`] set (logged at ERROR), because
/// refusing to boot would recover nothing.
pub async fn migrate_atlas_managed_tables(client: &mut Client) -> AtlasManagedMoveReport {
    run_pass(
        client,
        LEGACY_SCHEMA,
        ATLAS_MANAGED_SCHEMA,
        DEFAULT_LOCK_TIMEOUT,
    )
    .await
}

/// [`migrate_atlas_managed_tables`] between arbitrary schemas with an explicit
/// lock timeout, so the Postgres-backed tests can use scratch schemas.
pub(crate) async fn run_pass(
    client: &mut Client,
    source: &str,
    target: &str,
    lock_timeout: Duration,
) -> AtlasManagedMoveReport {
    let report = match migrate_between(client, source, target, lock_timeout).await {
        Ok(tables) => AtlasManagedMoveReport {
            tables,
            pass_error: None,
        },
        Err(e) => AtlasManagedMoveReport {
            tables: Vec::new(),
            pass_error: Some(e),
        },
    };
    log_report(source, target, &report);
    report
}

async fn migrate_between(
    client: &mut Client,
    source: &str,
    target: &str,
    lock_timeout: Duration,
) -> Result<Vec<(&'static str, TableMoveOutcome)>, String> {
    for schema in [source, target] {
        if !is_plain_identifier(schema) {
            return Err(format!("refusing non-identifier schema name {schema:?}"));
        }
    }

    // Fast path: one catalog read. Nothing legacy left means nothing to lock,
    // create or move.
    let present = client
        .query(
            "SELECT n.nspname::text, c.relname::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname IN ($1, $2) AND c.relname = ANY($3) AND c.relkind IN ('r', 'p')",
            &[&source, &target, &ATLAS_MANAGED_TABLES.as_slice()],
        )
        .await
        .map_err(|e| format!("atlas_managed move: catalog probe failed: {e}"))?;
    let mut in_source = Vec::new();
    let mut in_target = Vec::new();
    for row in &present {
        let schema: String = row.try_get(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get(1).map_err(|e| e.to_string())?;
        if schema == source {
            in_source.push(table);
        } else {
            in_target.push(table);
        }
    }
    if in_source.is_empty() {
        return Ok(ATLAS_MANAGED_TABLES
            .iter()
            .map(|t| {
                let outcome = if in_target.iter().any(|x| x == t) {
                    TableMoveOutcome::AlreadyInTarget
                } else {
                    TableMoveOutcome::Absent
                };
                (*t, outcome)
            })
            .collect());
    }

    let mut tx = client
        .transaction()
        .await
        .map_err(|e| format!("atlas_managed move: BEGIN failed: {e}"))?;
    // search_path = pg_catalog so pg_get_constraintdef schema-qualifies every
    // table it prints; every statement below is qualified anyway. All three
    // settings are LOCAL: they end with this transaction.
    tx.batch_execute(&format!(
        "SET LOCAL lock_timeout = '{}ms'; SET LOCAL statement_timeout = '{}ms'; \
         SET LOCAL search_path = pg_catalog;",
        lock_timeout.as_millis(),
        STATEMENT_TIMEOUT.as_millis()
    ))
    .await
    .map_err(|e| format!("atlas_managed move: session settings failed: {e}"))?;
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&MOVE_LOCK_KEY])
        .await
        .map_err(|e| format!("atlas_managed move: advisory lock failed: {e}"))?;
    tx.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS \"{target}\""))
        .await
        .map_err(|e| format!("atlas_managed move: CREATE SCHEMA {target} failed: {e}"))?;

    // Tables whose legacy copy cannot be dropped are excluded from merging and
    // the pass re-run, so no copy is committed for a legacy table that stays.
    // Each round excludes at least one more table, so this ends within six.
    let mut excluded: Vec<(&'static str, String)> = Vec::new();
    let outcomes = loop {
        let mut attempt = tx
            .savepoint("atlas_managed_attempt")
            .await
            .map_err(|e| format!("atlas_managed move: SAVEPOINT failed: {e}"))?;
        let (outcomes, refused) = attempt_pass(&mut attempt, source, target, &excluded).await?;
        if refused.is_empty() {
            attempt
                .commit()
                .await
                .map_err(|e| format!("atlas_managed move: RELEASE failed: {e}"))?;
            break outcomes;
        }
        attempt
            .rollback()
            .await
            .map_err(|e| format!("atlas_managed move: ROLLBACK TO SAVEPOINT failed: {e}"))?;
        excluded.extend(refused);
        if excluded.len() > ATLAS_MANAGED_TABLES.len() {
            return Err("atlas_managed move: drop exclusions did not converge".to_string());
        }
    };

    tx.commit()
        .await
        .map_err(|e| format!("atlas_managed move: COMMIT failed: {e}"))?;
    Ok(outcomes)
}

type Refusals = Vec<(&'static str, String)>;

/// One attempt at the whole pass inside a savepoint. Returns the outcomes and
/// the verified tables whose legacy drop was refused (the caller rolls the
/// attempt back and retries with those excluded).
async fn attempt_pass(
    tx: &mut Transaction<'_>,
    source: &str,
    target: &str,
    excluded: &[(&'static str, String)],
) -> Result<(Vec<(&'static str, TableMoveOutcome)>, Refusals), String> {
    let mut outcomes: Vec<(&'static str, TableMoveOutcome)> = Vec::new();
    let mut verified: Vec<(&'static str, i64, u64)> = Vec::new();

    for table in ATLAS_MANAGED_TABLES {
        let in_source = table_exists(tx, source, table).await?;
        let in_target = table_exists(tx, target, table).await?;
        let outcome = match (in_source, in_target) {
            (false, false) => TableMoveOutcome::Absent,
            (false, true) => TableMoveOutcome::AlreadyInTarget,
            (true, false) => move_table(tx, source, target, table).await?,
            (true, true) => {
                if let Some((_, reason)) = excluded.iter().find(|(t, _)| *t == table) {
                    let legacy_rows = count_rows(tx, source, table).await?;
                    let pk = primary_key(tx, target, table).await?;
                    let missing_rows = if pk.is_empty() {
                        -1
                    } else {
                        count_missing(tx, source, target, table, &pk).await?
                    };
                    TableMoveOutcome::KeptBoth {
                        legacy_rows,
                        missing_rows,
                        differing_rows: -1,
                        reason: reason.clone(),
                    }
                } else {
                    match merge_table(tx, source, target, table).await? {
                        MergeResult::Verified {
                            legacy_rows,
                            inserted,
                        } => {
                            // Recorded after the drop phase below decides its fate.
                            verified.push((table, legacy_rows, inserted));
                            continue;
                        }
                        MergeResult::Unverified(outcome) => outcome,
                    }
                }
            }
        };
        outcomes.push((table, outcome));
    }

    // Retire verified legacy copies children-first, so a child's FK into its
    // legacy parent is gone before the parent is dropped. RESTRICT, never
    // CASCADE: a drop some other legacy object still depends on is refused.
    let mut refused: Refusals = Vec::new();
    for (table, legacy_rows, inserted) in verified.iter().rev().copied() {
        let sp = tx.savepoint("atlas_managed_drop").await.map_err(|e| {
            format!("atlas_managed move: SAVEPOINT for dropping {table} failed: {e}")
        })?;
        match sp
            .batch_execute(&format!("DROP TABLE \"{source}\".\"{table}\" RESTRICT"))
            .await
        {
            Ok(()) => {
                sp.commit().await.map_err(|e| {
                    format!("atlas_managed move: RELEASE for dropping {table} failed: {e}")
                })?;
                outcomes.push((
                    table,
                    TableMoveOutcome::Merged {
                        legacy_rows,
                        inserted,
                    },
                ));
            }
            Err(e) => {
                sp.rollback().await.map_err(|e| {
                    format!("atlas_managed move: ROLLBACK for dropping {table} failed: {e}")
                })?;
                refused.push((
                    table,
                    format!(
                        "every row verified, but dropping {source}.{table} was refused \
                         ({}), so no copy is kept",
                        db_error_text(&e)
                    ),
                ));
            }
        }
    }
    outcomes.sort_by_key(|(t, _)| {
        ATLAS_MANAGED_TABLES
            .iter()
            .position(|x| x == t)
            .unwrap_or(usize::MAX)
    });
    Ok((outcomes, refused))
}

/// `SET SCHEMA` one legacy-only table inside a savepoint, re-pointing its FKs
/// at `atlas_managed` parents. Any failure rolls the move back.
async fn move_table(
    tx: &mut Transaction<'_>,
    source: &str,
    target: &str,
    table: &'static str,
) -> Result<TableMoveOutcome, String> {
    let mut sp = tx
        .savepoint("atlas_managed_move")
        .await
        .map_err(|e| format!("atlas_managed move: SAVEPOINT for moving {table} failed: {e}"))?;
    match move_and_repoint(&mut sp, source, target, table).await {
        Ok(repointed_fks) => {
            sp.commit().await.map_err(|e| {
                format!("atlas_managed move: RELEASE for moving {table} failed: {e}")
            })?;
            Ok(TableMoveOutcome::Moved { repointed_fks })
        }
        Err(reason) => {
            sp.rollback().await.map_err(|e| {
                format!("atlas_managed move: ROLLBACK for moving {table} failed: {e}")
            })?;
            Ok(TableMoveOutcome::NotMoved { reason })
        }
    }
}

async fn move_and_repoint(
    sp: &mut Transaction<'_>,
    source: &str,
    target: &str,
    table: &str,
) -> Result<u32, String> {
    sp.batch_execute(&format!(
        "ALTER TABLE \"{source}\".\"{table}\" SET SCHEMA \"{target}\""
    ))
    .await
    .map_err(|e| format!("SET SCHEMA failed: {}", db_error_text(&e)))?;

    // FKs of the moved table that still reference a `source` parent.
    let fks = sp
        .query(
            "SELECT con.conname::text, pg_get_constraintdef(con.oid), pc.relname::text \
             FROM pg_constraint con \
             JOIN pg_class pc ON pc.oid = con.confrelid \
             JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
             WHERE con.conrelid = format('%I.%I', $1::text, $2::text)::regclass \
               AND con.contype = 'f' AND pn.nspname = $3",
            &[&target, &table, &source],
        )
        .await
        .map_err(|e| format!("reading FKs failed: {}", db_error_text(&e)))?;

    let mut repointed = 0u32;
    for row in &fks {
        let name: String = row.try_get(0).map_err(|e| e.to_string())?;
        let def: String = row.try_get(1).map_err(|e| e.to_string())?;
        let parent: String = row.try_get(2).map_err(|e| e.to_string())?;
        if !table_exists(sp, target, &parent).await? {
            // The parent has no atlas_managed twin; the FK stays on it.
            continue;
        }
        let new_def = repoint_fk_definition(&def, source, target, &parent).ok_or_else(|| {
            format!("cannot re-point FK {name} ({def}) from {source}.{parent} to {target}.{parent}")
        })?;
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        sp.batch_execute(&format!(
            "ALTER TABLE \"{target}\".\"{table}\" DROP CONSTRAINT {quoted}; \
             ALTER TABLE \"{target}\".\"{table}\" ADD CONSTRAINT {quoted} {new_def} NOT VALID; \
             ALTER TABLE \"{target}\".\"{table}\" VALIDATE CONSTRAINT {quoted};"
        ))
        .await
        .map_err(|e| {
            format!(
                "FK {name} does not validate against {target}.{parent}: {}",
                db_error_text(&e)
            )
        })?;
        repointed += 1;
    }
    Ok(repointed)
}

/// Rewrite a `pg_get_constraintdef` FK definition (printed with
/// `search_path = pg_catalog`, so the parent is schema-qualified) to reference
/// `target.parent` instead of `source.parent`. `None` when the definition does
/// not name `source.parent` exactly once.
pub(crate) fn repoint_fk_definition(
    def: &str,
    source: &str,
    target: &str,
    parent: &str,
) -> Option<String> {
    let from = format!(" REFERENCES {source}.{parent}(");
    if def.matches(&from).count() != 1 {
        return None;
    }
    let to = format!(" REFERENCES {target}.{parent}(");
    let rewritten = def.replacen(&from, &to, 1);
    Some(
        rewritten
            .strip_suffix(" NOT VALID")
            .unwrap_or(&rewritten)
            .to_string(),
    )
}

enum MergeResult {
    Verified { legacy_rows: i64, inserted: u64 },
    Unverified(TableMoveOutcome),
}

/// Copy every legacy row into the target inside a savepoint and verify it;
/// the copy is committed only when verified. Never drops anything.
async fn merge_table(
    tx: &mut Transaction<'_>,
    source: &str,
    target: &str,
    table: &'static str,
) -> Result<MergeResult, String> {
    let target_cols = columns(tx, target, table).await?;
    let source_cols = columns(tx, source, table).await?;
    let legacy_rows = count_rows(tx, source, table).await?;
    let pk = primary_key(tx, target, table).await?;

    let kept = |missing_rows: i64, differing_rows: i64, reason: String| {
        Ok(MergeResult::Unverified(TableMoveOutcome::KeptBoth {
            legacy_rows,
            missing_rows,
            differing_rows,
            reason,
        }))
    };

    let plan = match plan_merge_columns(table, &target_cols, &source_cols) {
        Ok(plan) => plan,
        Err(reason) => return kept(-1, -1, reason),
    };
    if pk.is_empty() {
        return kept(
            -1,
            -1,
            format!("{target}.{table} has no primary key to verify the merge by"),
        );
    }
    if let Some(col) = pk
        .iter()
        .find(|c| !source_cols.iter().any(|s| &s.name == *c))
    {
        return kept(
            -1,
            -1,
            format!("legacy {source}.{table} lacks primary-key column {col}"),
        );
    }

    let insert_sql = format!(
        "INSERT INTO \"{target}\".\"{table}\" ({cols}) SELECT {exprs} FROM \"{source}\".\"{table}\" s \
         ON CONFLICT DO NOTHING",
        cols = plan
            .iter()
            .map(|(c, _)| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", "),
        exprs = plan
            .iter()
            .map(|(_, e)| e.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    let shared: Vec<String> = source_cols.iter().map(|c| c.name.clone()).collect();

    let sp = tx
        .savepoint("atlas_managed_merge")
        .await
        .map_err(|e| format!("atlas_managed move: SAVEPOINT for {table} failed: {e}"))?;
    let inserted = match sp.execute(insert_sql.as_str(), &[]).await {
        Ok(n) => n,
        Err(e) => {
            sp.rollback()
                .await
                .map_err(|e| format!("atlas_managed move: ROLLBACK for {table} failed: {e}"))?;
            return kept(
                -1,
                -1,
                format!("copying legacy rows failed: {}", db_error_text(&e)),
            );
        }
    };
    let missing_rows = count_missing(&sp, source, target, table, &pk).await?;
    let differing_rows = count_differing(&sp, source, target, table, &pk, &shared).await?;
    if missing_rows == 0 && differing_rows == 0 {
        sp.commit()
            .await
            .map_err(|e| format!("atlas_managed move: RELEASE for {table} failed: {e}"))?;
        Ok(MergeResult::Verified {
            legacy_rows,
            inserted,
        })
    } else {
        sp.rollback()
            .await
            .map_err(|e| format!("atlas_managed move: ROLLBACK for {table} failed: {e}"))?;
        kept(
            missing_rows,
            differing_rows,
            format!(
                "of {legacy_rows} legacy row(s), {missing_rows} have no primary-key match in \
                 {target}.{table} after the copy (a conflicting unique key kept them out) and \
                 {differing_rows} match a target row by primary key but differ in content; \
                 the copy was rolled back"
            ),
        )
    }
}

/// One column of a table, as the catalog reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnInfo {
    pub name: String,
    pub nullable: bool,
    pub has_default: bool,
}

/// The `(target column, SELECT expression over legacy alias s)` pairs of the
/// merge INSERT, in the target's column order. Column order is taken from the
/// catalog rather than `SELECT *`, because the two copies may have been built
/// with different column orders (e.g. `app_id` appended by `ADD COLUMN`).
///
/// Refuses (`Err` with the reason) when the merge could lose data or cannot
/// succeed: a legacy column the target lacks, or a NOT NULL target column
/// with no default that the legacy copy lacks and no known fill covers.
pub(crate) fn plan_merge_columns(
    table: &str,
    target: &[ColumnInfo],
    source: &[ColumnInfo],
) -> Result<Vec<(String, String)>, String> {
    if let Some(extra) = source
        .iter()
        .find(|s| !target.iter().any(|t| t.name == s.name))
    {
        return Err(format!(
            "legacy {table} has column {} that the atlas_managed copy lacks; merging would lose it",
            extra.name
        ));
    }
    let mut plan = Vec::with_capacity(target.len());
    for col in target {
        if source.iter().any(|s| s.name == col.name) {
            plan.push((col.name.clone(), format!("s.\"{}\"", col.name)));
        } else if let Some((_, _, fill)) = LEGACY_COLUMN_FILLS
            .iter()
            .find(|(t, c, _)| *t == table && *c == col.name)
        {
            plan.push((col.name.clone(), (*fill).to_string()));
        } else if col.nullable || col.has_default {
            // Let the target's default (or NULL) apply.
        } else {
            return Err(format!(
                "atlas_managed {table}.{} is NOT NULL with no default and absent from the legacy copy",
                col.name
            ));
        }
    }
    Ok(plan)
}

fn is_plain_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

fn db_error_text(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|db| format!("{} (SQLSTATE {})", db.message(), db.code().code()))
        .unwrap_or_else(|| e.to_string())
}

/// Whether `schema.table` exists as an ordinary or partitioned TABLE (a view,
/// sequence or index of that name does not count).
async fn table_exists<C: GenericClient + Sync>(
    c: &C,
    schema: &str,
    table: &str,
) -> Result<bool, String> {
    let row = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p'))",
            &[&schema, &table],
        )
        .await
        .map_err(|e| format!("atlas_managed move: probing {schema}.{table} failed: {e}"))?;
    row.try_get::<_, bool>(0)
        .map_err(|e| format!("atlas_managed move: probing {schema}.{table}: {e}"))
}

async fn columns<C: GenericClient + Sync>(
    c: &C,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let rows = c
        .query(
            "SELECT column_name::text, is_nullable = 'YES', column_default IS NOT NULL \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position",
            &[&schema, &table],
        )
        .await
        .map_err(|e| {
            format!("atlas_managed move: reading columns of {schema}.{table} failed: {e}")
        })?;
    rows.iter()
        .map(|r| {
            Ok(ColumnInfo {
                name: r.try_get(0).map_err(|e| e.to_string())?,
                nullable: r.try_get(1).map_err(|e| e.to_string())?,
                has_default: r.try_get(2).map_err(|e| e.to_string())?,
            })
        })
        .collect()
}

async fn primary_key<C: GenericClient + Sync>(
    c: &C,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, String> {
    let rows = c
        .query(
            "SELECT a.attname::text \
             FROM pg_index i \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
             WHERE i.indrelid = format('%I.%I', $1::text, $2::text)::regclass AND i.indisprimary \
             ORDER BY array_position(i.indkey, a.attnum)",
            &[&schema, &table],
        )
        .await
        .map_err(|e| {
            format!("atlas_managed move: reading the PK of {schema}.{table} failed: {e}")
        })?;
    rows.iter()
        .map(|r| r.try_get::<_, String>(0).map_err(|e| e.to_string()))
        .collect()
}

async fn count_rows<C: GenericClient + Sync>(
    c: &C,
    schema: &str,
    table: &str,
) -> Result<i64, String> {
    let row = c
        .query_one(
            &format!("SELECT count(*)::bigint FROM \"{schema}\".\"{table}\""),
            &[],
        )
        .await
        .map_err(|e| format!("atlas_managed move: counting {schema}.{table} failed: {e}"))?;
    row.try_get(0).map_err(|e| e.to_string())
}

fn pk_join(pk: &[String]) -> String {
    pk.iter()
        .map(|col| format!("a.\"{col}\" = s.\"{col}\""))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Legacy rows with no primary-key match in the target.
async fn count_missing<C: GenericClient + Sync>(
    c: &C,
    source: &str,
    target: &str,
    table: &str,
    pk: &[String],
) -> Result<i64, String> {
    let row = c
        .query_one(
            &format!(
                "SELECT count(*)::bigint FROM \"{source}\".\"{table}\" s \
                 WHERE NOT EXISTS (SELECT 1 FROM \"{target}\".\"{table}\" a WHERE {join})",
                join = pk_join(pk)
            ),
            &[],
        )
        .await
        .map_err(|e| format!("atlas_managed move: verifying {table} failed: {e}"))?;
    row.try_get(0).map_err(|e| e.to_string())
}

/// Legacy rows whose primary key matches a target row but whose value in any
/// of `shared` columns differs. Compared as text so every column type (json
/// included, which has no equality operator) is comparable.
async fn count_differing<C: GenericClient + Sync>(
    c: &C,
    source: &str,
    target: &str,
    table: &str,
    pk: &[String],
    shared: &[String],
) -> Result<i64, String> {
    let side = |alias: &str| {
        shared
            .iter()
            .map(|col| format!("{alias}.\"{col}\"::text"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let row = c
        .query_one(
            &format!(
                "SELECT count(*)::bigint FROM \"{source}\".\"{table}\" s \
                 JOIN \"{target}\".\"{table}\" a ON {join} \
                 WHERE ROW({t}) IS DISTINCT FROM ROW({s})",
                join = pk_join(pk),
                t = side("a"),
                s = side("s"),
            ),
            &[],
        )
        .await
        .map_err(|e| format!("atlas_managed move: comparing {table} failed: {e}"))?;
    row.try_get(0).map_err(|e| e.to_string())
}

fn log_report(source: &str, target: &str, report: &AtlasManagedMoveReport) {
    if let Some(e) = &report.pass_error {
        error!(
            error = e.as_str(),
            "atlas_managed move: the pass FAILED and was rolled back; boot continues. Any \
             {source}.* copy of an Atlas-owned table is still there, and runner queries read \
             only {target}.*"
        );
        return;
    }
    for (table, outcome) in &report.tables {
        match outcome {
            TableMoveOutcome::Absent => {
                debug!(table, "atlas_managed move: table absent in both schemas")
            }
            TableMoveOutcome::AlreadyInTarget => {
                debug!(table, "atlas_managed move: already in {target}")
            }
            TableMoveOutcome::Moved { repointed_fks } => info!(
                table,
                repointed_fks, "atlas_managed move: moved {source}.{table} into {target}"
            ),
            TableMoveOutcome::NotMoved { reason } => error!(
                table,
                reason = reason.as_str(),
                "atlas_managed move: {source}.{table} could NOT be moved into {target} and was \
                 left untouched; runner queries read only {target}.{table}"
            ),
            TableMoveOutcome::Merged {
                legacy_rows,
                inserted,
            } => info!(
                table,
                legacy_rows,
                inserted,
                "atlas_managed move: merged {source}.{table} into {target} (every row verified) \
                 and dropped the legacy copy"
            ),
            TableMoveOutcome::KeptBoth {
                legacy_rows,
                missing_rows,
                differing_rows,
                reason,
            } => error!(
                table,
                legacy_rows,
                missing_rows,
                differing_rows,
                reason = reason.as_str(),
                "atlas_managed move: {source}.{table} and {target}.{table} BOTH exist and the \
                 legacy copy was KEPT; nothing was copied or dropped. Reconcile by hand — runner \
                 queries read only {target}.{table}"
            ),
        }
    }
    let left = report.left_behind().count();
    if left > 0 {
        warn!(
            left_behind = left,
            "atlas_managed move: pass complete with legacy table(s) left in {source}"
        );
    } else if report.changed_anything() {
        info!("atlas_managed move: pass complete");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, nullable: bool, has_default: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            nullable,
            has_default,
        }
    }

    /// The move names exactly the six tables schema.hcl places in
    /// `atlas_managed`, and orders every FK parent before its children.
    #[test]
    fn table_list_is_the_six_hcl_tables_in_fk_order() {
        const HCL: &str = include_str!("../../../../atlas/schema.hcl");
        let mut declared = Vec::new();
        let mut current: Option<String> = None;
        for line in HCL.lines() {
            if let Some(rest) = line.strip_prefix("table \"") {
                current = rest.split('"').next().map(str::to_string);
            } else if let Some(t) = current.as_ref() {
                if line.trim() == "schema = schema.atlas_managed" {
                    declared.push(t.clone());
                    current = None;
                } else if line.trim_start().starts_with("schema =") {
                    current = None;
                }
            }
        }
        let mut ours: Vec<String> = ATLAS_MANAGED_TABLES.iter().map(|s| s.to_string()).collect();
        let mut hcl = declared.clone();
        ours.sort();
        hcl.sort();
        assert_eq!(
            ours, hcl,
            "ATLAS_MANAGED_TABLES must equal schema.hcl's atlas_managed tables"
        );

        // (child, parent) FK edges declared in schema.hcl.
        let pos = |t: &str| ATLAS_MANAGED_TABLES.iter().position(|x| *x == t).unwrap();
        for (child, parent) in [
            ("regression_runs", "regression_suites"),
            ("regression_diagnoses", "regression_runs"),
            ("regression_assertion_executions", "regression_runs"),
        ] {
            assert!(
                HCL.contains(&format!("ref_columns = [table.{parent}.column.id]")),
                "schema.hcl no longer declares the {child} -> {parent} FK; update this test"
            );
            assert!(pos(parent) < pos(child), "{parent} must precede {child}");
        }
    }

    /// No SQL in the modules that address the six tables may name them in
    /// `project` any more, and every FROM/INTO/UPDATE/JOIN/TABLE reference must
    /// be schema-qualified: `atlas_managed` is deliberately NOT on the
    /// search_path, so a bare name would fail at runtime.
    #[test]
    fn call_sites_address_atlas_managed_only() {
        let sources: [(&str, &str); 6] = [
            ("pg/regression.rs", include_str!("regression.rs")),
            ("pg/spec_proposals.rs", include_str!("spec_proposals.rs")),
            ("pg/proposal_events.rs", include_str!("proposal_events.rs")),
            ("pg/mod.rs", include_str!("mod.rs")),
            (
                "bin/qontinui_specs.rs",
                include_str!("../../bin/qontinui_specs.rs"),
            ),
            (
                "queries/regression.sql",
                include_str!("../../../queries/regression.sql"),
            ),
        ];
        let keywords = ["FROM", "INTO", "UPDATE", "JOIN", "TABLE"];
        let mut offenders = Vec::new();
        for (file, text) in sources {
            for (n, line) in text.lines().enumerate() {
                for table in ATLAS_MANAGED_TABLES {
                    if line.contains(&format!("project.{table}")) {
                        offenders.push(format!("{file}:{}: project.{table}", n + 1));
                    }
                    let tokens: Vec<&str> = line.split_whitespace().collect();
                    for w in tokens.windows(2) {
                        // Last alphabetic run, so `r#"UPDATE` reads as UPDATE.
                        let kw = w[0]
                            .rsplit(|c: char| !c.is_ascii_alphabetic())
                            .next()
                            .unwrap_or("")
                            .to_ascii_uppercase();
                        let name = w[1]
                            .trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
                        if keywords.contains(&kw.as_str()) && name == table {
                            offenders.push(format!("{file}:{}: bare `{kw} {table}`", n + 1));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "unqualified or project.* references:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn plan_uses_target_column_order_not_source_order() {
        let target = [
            col("id", false, false),
            col("a", true, false),
            col("b", false, false),
        ];
        let source = [
            col("b", false, false),
            col("id", false, false),
            col("a", true, false),
        ];
        let plan = plan_merge_columns("regression_suites", &target, &source).unwrap();
        assert_eq!(
            plan,
            vec![
                ("id".to_string(), "s.\"id\"".to_string()),
                ("a".to_string(), "s.\"a\"".to_string()),
                ("b".to_string(), "s.\"b\"".to_string()),
            ]
        );
    }

    #[test]
    fn plan_fills_app_id_for_a_pre_multi_app_legacy_copy() {
        let target = [col("id", false, false), col("app_id", false, false)];
        let source = [col("id", false, false)];
        let plan = plan_merge_columns("proposal_events", &target, &source).unwrap();
        assert_eq!(
            plan[1],
            ("app_id".to_string(), "'qontinui-runner'".to_string())
        );
    }

    #[test]
    fn plan_lets_defaults_and_nulls_apply_for_columns_the_legacy_copy_lacks() {
        let target = [
            col("id", false, false),
            col("created_at", false, true),
            col("note", true, false),
        ];
        let source = [col("id", false, false)];
        let plan = plan_merge_columns("regression_suites", &target, &source).unwrap();
        assert_eq!(plan, vec![("id".to_string(), "s.\"id\"".to_string())]);
    }

    #[test]
    fn plan_refuses_a_legacy_column_the_target_lacks() {
        let target = [col("id", false, false)];
        let source = [col("id", false, false), col("extra", true, false)];
        let err = plan_merge_columns("regression_suites", &target, &source).unwrap_err();
        assert!(err.contains("extra"), "{err}");
    }

    #[test]
    fn plan_refuses_an_unfillable_not_null_column() {
        let target = [col("id", false, false), col("must", false, false)];
        let source = [col("id", false, false)];
        let err = plan_merge_columns("regression_suites", &target, &source).unwrap_err();
        assert!(err.contains("must"), "{err}");
    }

    #[test]
    fn identifier_guard_accepts_schema_names_and_rejects_injection() {
        assert!(is_plain_identifier("atlas_managed"));
        assert!(is_plain_identifier("project"));
        assert!(!is_plain_identifier("project\"; DROP SCHEMA x; --"));
        assert!(!is_plain_identifier(""));
        assert!(!is_plain_identifier("1abc"));
    }

    #[test]
    fn fk_definition_is_repointed_at_the_target_parent() {
        let def =
            "FOREIGN KEY (suite_id) REFERENCES project.regression_suites(id) ON DELETE CASCADE";
        assert_eq!(
            repoint_fk_definition(def, "project", "atlas_managed", "regression_suites").as_deref(),
            Some(
                "FOREIGN KEY (suite_id) REFERENCES atlas_managed.regression_suites(id) ON DELETE CASCADE"
            )
        );
        let not_valid = format!("{def} NOT VALID");
        assert!(!repoint_fk_definition(
            &not_valid,
            "project",
            "atlas_managed",
            "regression_suites"
        )
        .unwrap()
        .ends_with("NOT VALID"));
        // An unqualified or differently-named parent is refused, never guessed.
        assert_eq!(
            repoint_fk_definition(
                "FOREIGN KEY (suite_id) REFERENCES regression_suites(id)",
                "project",
                "atlas_managed",
                "regression_suites"
            ),
            None
        );
    }

    #[test]
    fn report_counts_left_behind_tables() {
        let report = AtlasManagedMoveReport {
            tables: vec![
                ("regression_suites", TableMoveOutcome::AlreadyInTarget),
                (
                    "regression_runs",
                    TableMoveOutcome::NotMoved { reason: "x".into() },
                ),
                (
                    "spec_proposals",
                    TableMoveOutcome::KeptBoth {
                        legacy_rows: 1,
                        missing_rows: 1,
                        differing_rows: 0,
                        reason: "y".into(),
                    },
                ),
            ],
            pass_error: None,
        };
        assert_eq!(report.left_behind().count(), 2);
        assert!(report.changed_anything());
    }

    /// Postgres-backed boot scenarios against scratch schemas on the
    /// `DATABASE_URL` fixture (same fixture `PgDb::new_for_test` uses). Needs a
    /// reachable Postgres, hence the feature gate.
    ///
    /// Run: `cargo test --features pg_integration_tests -- atlas_managed_move::tests::pg`
    #[cfg(feature = "pg_integration_tests")]
    mod pg {
        use super::super::*;

        const ALL: &[&str] = &ATLAS_MANAGED_TABLES;

        fn url() -> String {
            std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string())
        }

        async fn connect() -> Client {
            let (client, conn) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
                .await
                .expect("connect to DATABASE_URL");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            client
        }

        struct Scratch {
            client: Client,
            source: String,
            target: String,
        }

        impl Scratch {
            async fn new() -> Self {
                let tag: String = uuid::Uuid::new_v4()
                    .simple()
                    .to_string()
                    .chars()
                    .take(12)
                    .collect();
                Self {
                    client: connect().await,
                    source: format!("amv_src_{tag}"),
                    target: format!("amv_dst_{tag}"),
                }
            }

            /// Create `tables` in `schema`. `legacy` builds the older shape:
            /// columns in a different order, and `proposal_events` without
            /// `app_id`. FKs reference the parent in the SAME schema.
            async fn create(&self, schema: &str, legacy: bool, tables: &[&str]) {
                let mut ddl = format!("CREATE SCHEMA IF NOT EXISTS {schema};");
                for t in tables {
                    let body = match *t {
                        "regression_suites" if legacy => {
                            "suite_json JSONB NOT NULL, ir_doc_id TEXT NOT NULL, id UUID PRIMARY KEY, \
                             created_at TIMESTAMPTZ NOT NULL DEFAULT now()"
                                .to_string()
                        }
                        "regression_suites" => "id UUID PRIMARY KEY, ir_doc_id TEXT NOT NULL, \
                             suite_json JSONB NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now()"
                            .to_string(),
                        "regression_runs" => format!(
                            "id UUID PRIMARY KEY, suite_id UUID NOT NULL REFERENCES \
                             {schema}.regression_suites(id) ON DELETE CASCADE, run_id TEXT NOT NULL"
                        ),
                        "regression_diagnoses" => format!(
                            "id UUID PRIMARY KEY, run_id UUID NOT NULL REFERENCES \
                             {schema}.regression_runs(id) ON DELETE CASCADE, diagnosis_json JSONB NOT NULL"
                        ),
                        "regression_assertion_executions" => format!(
                            "id UUID PRIMARY KEY, run_id UUID NOT NULL REFERENCES \
                             {schema}.regression_runs(id) ON DELETE CASCADE, case_id TEXT NOT NULL"
                        ),
                        "spec_proposals" => "id TEXT PRIMARY KEY, kind TEXT NOT NULL, pathname TEXT, \
                             spec_id TEXT, status TEXT NOT NULL"
                            .to_string(),
                        "proposal_events" if legacy => {
                            "id TEXT PRIMARY KEY, proposal_id TEXT NOT NULL, event_type TEXT NOT NULL"
                                .to_string()
                        }
                        "proposal_events" => "id TEXT PRIMARY KEY, proposal_id TEXT NOT NULL, \
                             event_type TEXT NOT NULL, app_id TEXT NOT NULL"
                            .to_string(),
                        other => panic!("unknown table {other}"),
                    };
                    ddl.push_str(&format!("CREATE TABLE {schema}.{t} ({body});"));
                    if *t == "spec_proposals" {
                        ddl.push_str(&format!(
                            "CREATE UNIQUE INDEX ON {schema}.spec_proposals (kind, (COALESCE(pathname, spec_id)));"
                        ));
                    }
                }
                self.client
                    .batch_execute(&ddl)
                    .await
                    .expect("create scratch tables");
            }

            async fn exec(&self, sql: &str) {
                self.client.batch_execute(sql).await.expect(sql);
            }

            /// One row in each of `tables` (FK chain through `suite`/`run`).
            async fn seed(
                &self,
                schema: &str,
                suite: uuid::Uuid,
                run: uuid::Uuid,
                tables: &[&str],
                with_app: bool,
            ) {
                let mut sql = String::new();
                for t in tables {
                    sql.push_str(&match *t {
                        "regression_suites" => format!("INSERT INTO {schema}.regression_suites (id, ir_doc_id, suite_json) VALUES ('{suite}', 'doc', '{{}}');"),
                        "regression_runs" => format!("INSERT INTO {schema}.regression_runs (id, suite_id, run_id) VALUES ('{run}', '{suite}', 'r');"),
                        "regression_diagnoses" => format!("INSERT INTO {schema}.regression_diagnoses (id, run_id, diagnosis_json) VALUES (gen_random_uuid(), '{run}', '{{}}');"),
                        "regression_assertion_executions" => format!("INSERT INTO {schema}.regression_assertion_executions (id, run_id, case_id) VALUES (gen_random_uuid(), '{run}', 'c');"),
                        "spec_proposals" => format!("INSERT INTO {schema}.spec_proposals (id, kind, pathname, status) VALUES ('sp-{run}', 'fullPage', '/{run}', 'queued');"),
                        "proposal_events" if with_app => format!("INSERT INTO {schema}.proposal_events (id, proposal_id, event_type, app_id) VALUES ('pe-{run}', 'p', 'scanned', 'app-x');"),
                        "proposal_events" => format!("INSERT INTO {schema}.proposal_events (id, proposal_id, event_type) VALUES ('pe-{run}', 'p', 'scanned');"),
                        other => panic!("unknown table {other}"),
                    });
                }
                self.exec(&sql).await;
            }

            async fn count(&self, schema: &str, table: &str) -> i64 {
                self.client
                    .query_one(
                        &format!("SELECT count(*)::bigint FROM {schema}.{table}"),
                        &[],
                    )
                    .await
                    .expect("count")
                    .try_get(0)
                    .expect("count value")
            }

            async fn exists(&self, schema: &str, table: &str) -> bool {
                table_exists(&self.client, schema, table)
                    .await
                    .expect("probe")
            }

            /// The schema of the table `schema.table`'s FKs reference.
            async fn fk_parent_schemas(&self, schema: &str, table: &str) -> Vec<String> {
                self.client
                    .query(
                        "SELECT pn.nspname::text FROM pg_constraint con \
                         JOIN pg_class pc ON pc.oid = con.confrelid \
                         JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
                         WHERE con.conrelid = format('%I.%I', $1::text, $2::text)::regclass \
                           AND con.contype = 'f' AND con.convalidated",
                        &[&schema, &table],
                    )
                    .await
                    .expect("fk parents")
                    .iter()
                    .map(|r| r.try_get(0).unwrap())
                    .collect()
            }

            async fn run(&mut self) -> AtlasManagedMoveReport {
                let (s, t) = (self.source.clone(), self.target.clone());
                let report = run_pass(&mut self.client, &s, &t, DEFAULT_LOCK_TIMEOUT).await;
                assert_eq!(report.pass_error, None, "{report:?}");
                report
            }

            async fn cleanup(&self) {
                let _ = self
                    .client
                    .batch_execute(&format!(
                        "DROP SCHEMA IF EXISTS {} CASCADE; DROP SCHEMA IF EXISTS {} CASCADE;",
                        self.source, self.target
                    ))
                    .await;
            }
        }

        fn is_kept(o: Option<&TableMoveOutcome>) -> bool {
            matches!(o, Some(TableMoveOutcome::KeptBoth { .. }))
        }

        #[tokio::test]
        async fn legacy_only_tables_are_moved_with_their_rows() {
            let mut s = Scratch::new().await;
            let src = s.source.clone();
            s.create(&src, true, ALL).await;
            s.seed(&src, uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), ALL, false)
                .await;

            let report = s.run().await;
            for t in ATLAS_MANAGED_TABLES {
                // FKs follow the parent by OID when the parent moved too.
                assert_eq!(
                    report.outcome(t),
                    Some(&TableMoveOutcome::Moved { repointed_fks: 0 }),
                    "{t}"
                );
                assert!(!s.exists(&s.source, t).await, "{t} left in source");
                assert_eq!(s.count(&s.target, t).await, 1, "{t} rows");
            }

            let again = s.run().await;
            assert!(!again.changed_anything(), "{again:?}");
            s.cleanup().await;
        }

        #[tokio::test]
        async fn both_present_merges_every_row_then_drops_the_legacy_copy() {
            let mut s = Scratch::new().await;
            let (src, dst) = (s.source.clone(), s.target.clone());
            s.create(&src, true, ALL).await;
            s.create(&dst, false, ALL).await;
            s.seed(&src, uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), ALL, false)
                .await;
            s.seed(&dst, uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), ALL, true)
                .await;

            let report = s.run().await;
            for t in ATLAS_MANAGED_TABLES {
                assert!(
                    matches!(
                        report.outcome(t),
                        Some(TableMoveOutcome::Merged {
                            legacy_rows: 1,
                            inserted: 1
                        })
                    ),
                    "{t}: {:?}",
                    report.outcome(t)
                );
                assert!(!s.exists(&src, t).await, "{t} legacy copy not dropped");
                assert_eq!(s.count(&dst, t).await, 2, "{t} rows after merge");
            }
            let filled: i64 = s
                .client
                .query_one(
                    &format!("SELECT count(*)::bigint FROM {dst}.proposal_events WHERE app_id = 'qontinui-runner'"),
                    &[],
                )
                .await
                .unwrap()
                .try_get(0)
                .unwrap();
            assert_eq!(
                filled, 1,
                "legacy proposal_events row gets the bootstrap app_id"
            );

            let again = s.run().await;
            assert!(!again.changed_anything(), "{again:?}");
            s.cleanup().await;
        }

        /// A legacy row kept out by a conflicting unique key fails
        /// verification: nothing is copied (no partial merge to resurrect
        /// later) and nothing is dropped, on every boot.
        #[tokio::test]
        async fn unverifiable_merge_keeps_both_and_copies_nothing() {
            let mut s = Scratch::new().await;
            let (src, dst) = (s.source.clone(), s.target.clone());
            s.create(&src, true, &["spec_proposals"]).await;
            s.create(&dst, false, &["spec_proposals"]).await;
            s.exec(&format!(
                "INSERT INTO {src}.spec_proposals (id, kind, pathname, status) VALUES ('legacy', 'fullPage', '/same', 'queued'), ('other', 'patch', '/x', 'queued');
                 INSERT INTO {dst}.spec_proposals (id, kind, pathname, status) VALUES ('current', 'fullPage', '/same', 'queued');"
            ))
            .await;

            for _boot in 0..2 {
                let report = s.run().await;
                match report.outcome("spec_proposals") {
                    Some(TableMoveOutcome::KeptBoth {
                        legacy_rows: 2,
                        missing_rows: 1,
                        differing_rows: 0,
                        ..
                    }) => {}
                    other => panic!("expected KeptBoth, got {other:?}"),
                }
                assert_eq!(
                    s.count(&src, "spec_proposals").await,
                    2,
                    "legacy copy must survive"
                );
                assert_eq!(
                    s.count(&dst, "spec_proposals").await,
                    1,
                    "the verifiable row must NOT be half-merged"
                );
            }
            s.cleanup().await;
        }

        /// Same PK, different content: ON CONFLICT would skip the legacy row
        /// silently; the content check keeps both instead of dropping it.
        #[tokio::test]
        async fn same_key_different_content_keeps_both() {
            let mut s = Scratch::new().await;
            let (src, dst) = (s.source.clone(), s.target.clone());
            s.create(&src, true, &["spec_proposals"]).await;
            s.create(&dst, false, &["spec_proposals"]).await;
            s.exec(&format!(
                "INSERT INTO {src}.spec_proposals (id, kind, pathname, status) VALUES ('p1', 'fullPage', '/a', 'promoted');
                 INSERT INTO {dst}.spec_proposals (id, kind, pathname, status) VALUES ('p1', 'fullPage', '/a', 'queued');"
            ))
            .await;

            let report = s.run().await;
            match report.outcome("spec_proposals") {
                Some(TableMoveOutcome::KeptBoth {
                    missing_rows: 0,
                    differing_rows: 1,
                    ..
                }) => {}
                other => panic!("expected KeptBoth with one differing row, got {other:?}"),
            }
            assert!(s.exists(&src, "spec_proposals").await);
            let status: String = s
                .client
                .query_one(
                    &format!("SELECT status FROM {dst}.spec_proposals WHERE id = 'p1'"),
                    &[],
                )
                .await
                .unwrap()
                .try_get(0)
                .unwrap();
            assert_eq!(status, "queued", "the target row is untouched");
            s.cleanup().await;
        }

        /// Parent in both schemas (merged), child only in the legacy schema:
        /// the moved child's FK must be re-pointed at the target parent, or the
        /// legacy parent could never be dropped and target inserts would fail.
        #[tokio::test]
        async fn parent_in_both_child_only_in_legacy_repoints_the_fk() {
            let mut s = Scratch::new().await;
            let (src, dst) = (s.source.clone(), s.target.clone());
            let chain = &[
                "regression_suites",
                "regression_runs",
                "regression_diagnoses",
                "regression_assertion_executions",
            ];
            s.create(&src, true, chain).await;
            s.create(&dst, false, &["regression_suites"]).await;
            let (suite, run) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
            s.seed(&src, suite, run, chain, false).await;

            let report = s.run().await;
            assert!(
                matches!(
                    report.outcome("regression_suites"),
                    Some(TableMoveOutcome::Merged { legacy_rows: 1, .. })
                ),
                "{report:?}"
            );
            assert_eq!(
                report.outcome("regression_runs"),
                Some(&TableMoveOutcome::Moved { repointed_fks: 1 })
            );
            assert!(
                !s.exists(&src, "regression_suites").await,
                "legacy parent dropped"
            );
            assert_eq!(
                s.fk_parent_schemas(&dst, "regression_runs").await,
                vec![dst.clone()]
            );
            // A new child row against the target parent is accepted.
            s.exec(&format!(
                "INSERT INTO {dst}.regression_runs (id, suite_id, run_id) VALUES (gen_random_uuid(), '{suite}', 'r2');"
            ))
            .await;
            assert!(!s.run().await.changed_anything());
            s.cleanup().await;
        }

        /// Parent kept in both (unmergeable), child only in legacy: the child's
        /// FK does not validate against the target parent, so the move is rolled
        /// back and everything legacy stays intact.
        #[tokio::test]
        async fn child_fk_that_does_not_validate_is_not_moved() {
            let mut s = Scratch::new().await;
            let (src, dst) = (s.source.clone(), s.target.clone());
            s.create(&src, true, &["regression_suites", "regression_runs"])
                .await;
            s.create(&dst, false, &["regression_suites"]).await;
            s.seed(
                &src,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                &["regression_suites", "regression_runs"],
                false,
            )
            .await;
            s.exec(&format!(
                "ALTER TABLE {dst}.regression_suites ADD COLUMN must TEXT NOT NULL"
            ))
            .await;

            let report = s.run().await;
            assert!(is_kept(report.outcome("regression_suites")), "{report:?}");
            assert!(
                matches!(
                    report.outcome("regression_runs"),
                    Some(TableMoveOutcome::NotMoved { .. })
                ),
                "{report:?}"
            );
            assert!(s.exists(&src, "regression_runs").await);
            assert!(!s.exists(&dst, "regression_runs").await);
            assert_eq!(
                s.fk_parent_schemas(&src, "regression_runs").await,
                vec![src.clone()]
            );
            assert_eq!(s.count(&src, "regression_runs").await, 1);
            s.cleanup().await;
        }

        /// A verified parent whose legacy drop is refused (an unmergeable legacy
        /// child still references it) is re-run excluded: its rows are NOT left
        /// copied into the target.
        #[tokio::test]
        async fn refused_parent_drop_leaves_no_copy_behind() {
            let mut s = Scratch::new().await;
            let (src, dst) = (s.source.clone(), s.target.clone());
            s.create(&src, true, ALL).await;
            s.create(&dst, false, ALL).await;
            s.seed(&src, uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), ALL, false)
                .await;
            s.exec(&format!(
                "ALTER TABLE {dst}.regression_runs ADD COLUMN must TEXT NOT NULL"
            ))
            .await;

            let report = s.run().await;
            for t in [
                "regression_suites",
                "regression_runs",
                "regression_diagnoses",
                "regression_assertion_executions",
            ] {
                assert!(is_kept(report.outcome(t)), "{t}: {report:?}");
                assert!(s.exists(&src, t).await, "{t} legacy survives");
                assert_eq!(s.count(&dst, t).await, 0, "{t}: no copy committed");
            }
            // The independent tables still merge.
            assert!(matches!(
                report.outcome("spec_proposals"),
                Some(TableMoveOutcome::Merged { .. })
            ));
            s.cleanup().await;
        }

        #[tokio::test]
        async fn absent_everywhere_is_a_no_op() {
            let mut s = Scratch::new().await;
            let report = s.run().await;
            assert!(!report.changed_anything());
            for t in ATLAS_MANAGED_TABLES {
                assert_eq!(report.outcome(t), Some(&TableMoveOutcome::Absent));
            }
            // The fast path returns before CREATE SCHEMA.
            let target_schema: bool = s
                .client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)",
                    &[&s.target],
                )
                .await
                .unwrap()
                .try_get(0)
                .unwrap();
            assert!(
                !target_schema,
                "a steady-state pass must not create the schema"
            );
            s.cleanup().await;
        }

        /// A converged database is recognised by the catalog probe alone: the
        /// pass succeeds even while another session holds the advisory lock.
        #[tokio::test]
        async fn steady_state_needs_no_advisory_lock() {
            let mut s = Scratch::new().await;
            let dst = s.target.clone();
            s.create(&dst, false, ALL).await;
            let holder = connect().await;
            holder
                .execute("SELECT pg_advisory_lock($1)", &[&MOVE_LOCK_KEY])
                .await
                .unwrap();
            let (a, b) = (s.source.clone(), s.target.clone());
            let report = run_pass(&mut s.client, &a, &b, Duration::from_millis(200)).await;
            holder
                .execute("SELECT pg_advisory_unlock($1)", &[&MOVE_LOCK_KEY])
                .await
                .unwrap();
            assert_eq!(report.pass_error, None, "{report:?}");
            for t in ATLAS_MANAGED_TABLES {
                assert_eq!(report.outcome(t), Some(&TableMoveOutcome::AlreadyInTarget));
            }
            s.cleanup().await;
        }

        /// A pass that cannot run (here: the advisory lock is held elsewhere
        /// past the lock timeout) is reported, rolled back, and does not raise.
        #[tokio::test]
        async fn a_failed_pass_is_reported_not_raised() {
            let mut s = Scratch::new().await;
            let src = s.source.clone();
            s.create(&src, true, &["spec_proposals"]).await;
            let holder = connect().await;
            holder
                .execute("SELECT pg_advisory_lock($1)", &[&MOVE_LOCK_KEY])
                .await
                .unwrap();

            let (a, b) = (s.source.clone(), s.target.clone());
            let report = run_pass(&mut s.client, &a, &b, Duration::from_millis(200)).await;
            holder
                .execute("SELECT pg_advisory_unlock($1)", &[&MOVE_LOCK_KEY])
                .await
                .unwrap();

            let err = report.pass_error.clone().expect("pass error reported");
            assert!(err.contains("advisory lock"), "{err}");
            assert!(report.tables.is_empty());
            assert!(s.exists(&src, "spec_proposals").await, "nothing moved");
            assert!(!s.exists(&b, "spec_proposals").await);
            // The connection is usable afterwards (the transaction rolled back).
            assert_eq!(s.count(&src, "spec_proposals").await, 0);
            s.cleanup().await;
        }
    }
}
