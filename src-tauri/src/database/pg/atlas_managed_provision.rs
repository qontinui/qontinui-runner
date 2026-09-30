//! Boot-time creation of an Atlas-owned table that a database lacks entirely
//! (plan `2026-09-28-atlas-managed-follow-ups-embedded-provisioning-ci-and-script-hardening`,
//! item 1).
//!
//! A fresh embedded cluster gets all six `atlas_managed` tables from the
//! bundled `schema.pg.sql.generated`. An OLDER cluster does not: it was built
//! from a dump that carried only the four `regression_*` tables, and
//! `embedded_pg::apply_canonical_schema` never runs again once a cluster has a
//! schema. [`super::atlas_managed_move`] then moves those four into
//! `atlas_managed`, but it moves tables and never creates one, so
//! `spec_proposals` and `proposal_events` stay missing and every flywheel
//! spec-proposal path fails with `relation does not exist`.
//!
//! This step closes that gap without a second hand-written copy of
//! `atlas/schema.hcl`. The DDL it runs is Atlas's own: the `atlas_managed`
//! objects of the bundled dump, which every codegen pipeline regenerates
//! after `atlas schema apply` and `schema-pg-sql-fresh` keeps current. pg_dump
//! heads each object with `-- Name: …; Type: …; Schema: atlas_managed; …`,
//! so the objects are picked out by that header, not by guessing at SQL.
//!
//! A table is created only when it is absent from BOTH `atlas_managed` and
//! the legacy `project` schema. A legacy copy the move could not carry across
//! is left alone: creating an empty twin beside it would hide the leftover
//! behind a table that answers queries with no rows.
//!
//! It runs after the move, in one transaction under the move's own advisory
//! lock (`MOVE_LOCK_KEY`), so it can never interleave with a move running on
//! another connection, and it is never fatal. On a converged database it is
//! one catalog query and takes no lock.

use std::collections::HashSet;

use tokio_postgres::{Client, GenericClient};
use tracing::{error, info, warn};

use super::atlas_managed_move::{
    ATLAS_MANAGED_SCHEMA, ATLAS_MANAGED_TABLES, DEFAULT_LOCK_TIMEOUT, LEGACY_SCHEMA, MOVE_LOCK_KEY,
    STATEMENT_TIMEOUT,
};

/// The kinds of `atlas_managed` object the dump carries, in the order they are
/// applied: tables, then their primary keys, then indexes, then FKs (whose
/// parent must already exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ObjectKind {
    Table,
    Constraint,
    Index,
    ForeignKey,
}

impl ObjectKind {
    fn from_dump_type(t: &str) -> Option<Self> {
        match t {
            "TABLE" => Some(Self::Table),
            "CONSTRAINT" => Some(Self::Constraint),
            "INDEX" => Some(Self::Index),
            "FK CONSTRAINT" => Some(Self::ForeignKey),
            _ => None,
        }
    }
}

/// One `atlas_managed` object from the dump: the table it belongs to, its
/// kind, its statement exactly as pg_dump wrote it, and — for an FK — the
/// `atlas_managed` table it references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DumpObject {
    pub table: String,
    pub kind: ObjectKind,
    pub sql: String,
    pub parent: Option<String>,
}

/// Every `atlas_managed` object in `dump`, in dump order.
///
/// Refuses (rather than skips) an `atlas_managed` object of a kind it does not
/// know how to order, so a schema.hcl change that adds, say, a sequence fails
/// the unit test over the bundled dump instead of silently creating a table
/// without it.
pub(crate) fn atlas_managed_objects(dump: &str) -> Result<Vec<DumpObject>, String> {
    let schema_tag = format!("; Schema: {ATLAS_MANAGED_SCHEMA};");
    let qualifier = format!("{ATLAS_MANAGED_SCHEMA}.");
    let mut objects = Vec::new();
    let mut lines = dump.lines();

    while let Some(line) = lines.next() {
        let Some(header) = line.strip_prefix("-- Name: ") else {
            continue;
        };
        if !header.contains(&schema_tag) {
            continue;
        }
        let mut fields = header.split("; ");
        let name = fields.next().unwrap_or_default();
        let dump_type = fields
            .next()
            .and_then(|f| f.strip_prefix("Type: "))
            .unwrap_or_default();
        let kind = ObjectKind::from_dump_type(dump_type).ok_or_else(|| {
            format!("atlas_managed object {name:?} has unsupported dump type {dump_type:?}")
        })?;

        // The statement starts at the first line that is neither blank nor a
        // comment, and ends at the first line ending in `;`.
        let mut sql = String::new();
        for body in lines.by_ref() {
            if sql.is_empty() && (body.trim().is_empty() || body.starts_with("--")) {
                continue;
            }
            sql.push_str(body);
            sql.push('\n');
            if body.trim_end().ends_with(';') {
                break;
            }
        }
        if sql.is_empty() {
            return Err(format!("atlas_managed object {name:?} has no statement"));
        }

        let table = match kind {
            ObjectKind::Table => name.to_string(),
            // pg_dump names a constraint `<table> <constraint>`.
            ObjectKind::Constraint | ObjectKind::ForeignKey => {
                name.split(' ').next().unwrap_or_default().to_string()
            }
            // An index is named alone; its table follows `ON atlas_managed.`.
            ObjectKind::Index => sql
                .split(&format!(" ON {qualifier}"))
                .nth(1)
                .and_then(|rest| rest.split([' ', '(']).next())
                .unwrap_or_default()
                .to_string(),
        };
        if table.is_empty() {
            return Err(format!(
                "could not tell which table atlas_managed object {name:?} belongs to"
            ));
        }
        let parent = match kind {
            ObjectKind::ForeignKey => {
                let parent = sql
                    .split(&format!(" REFERENCES {qualifier}"))
                    .nth(1)
                    .and_then(|rest| rest.split([' ', '(']).next())
                    .unwrap_or_default();
                if parent.is_empty() {
                    return Err(format!(
                        "could not tell which atlas_managed table FK {name:?} references"
                    ));
                }
                Some(parent.to_string())
            }
            _ => None,
        };
        objects.push(DumpObject {
            table,
            kind,
            sql,
            parent,
        });
    }
    Ok(objects)
}

/// The statements that create `tables` in `target`, ordered tables →
/// constraints → indexes → FKs, and within each kind in dump order.
///
/// `parents` is every table that will exist in `target` once `tables` are
/// created. An FK whose parent is not among them is left out (see
/// [`skipped_foreign_keys`]): creating it would fail the whole transaction.
pub(crate) fn creation_statements(
    objects: &[DumpObject],
    tables: &[&str],
    parents: &[&str],
    target: &str,
) -> Vec<String> {
    let mut picked: Vec<&DumpObject> = objects
        .iter()
        .filter(|o| tables.contains(&o.table.as_str()))
        .filter(|o| {
            o.parent
                .as_deref()
                .is_none_or(|parent| parents.contains(&parent))
        })
        .collect();
    picked.sort_by_key(|o| o.kind);
    let from = format!("{ATLAS_MANAGED_SCHEMA}.");
    let to = format!("{target}.");
    picked
        .into_iter()
        .map(|o| o.sql.replace(&from, &to))
        .collect()
}

/// The FKs of `tables` that [`creation_statements`] leaves out because their
/// parent will not exist in the target, as `table -> parent`.
fn skipped_foreign_keys(objects: &[DumpObject], tables: &[&str], parents: &[&str]) -> Vec<String> {
    objects
        .iter()
        .filter(|o| tables.contains(&o.table.as_str()))
        .filter_map(|o| {
            let parent = o.parent.as_deref()?;
            (!parents.contains(&parent)).then(|| format!("{} -> {parent}", o.table))
        })
        .collect()
}

/// Which of the six tables exist as a table (`relkind` `r`/`p`, the move's
/// own `table_exists` predicate) in `target` and in `legacy`, in one query.
async fn presence<C: GenericClient + Sync>(
    client: &C,
    legacy: &str,
    target: &str,
) -> Result<(HashSet<String>, HashSet<String>), String> {
    let rows = client
        .query(
            "SELECT n.nspname::text, c.relname::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname IN ($1, $2) AND c.relname = ANY($3) AND c.relkind IN ('r', 'p')",
            &[&legacy, &target, &ATLAS_MANAGED_TABLES.as_slice()],
        )
        .await
        .map_err(|e| format!("catalog probe failed: {e}"))?;
    let (mut in_target, mut in_legacy) = (HashSet::new(), HashSet::new());
    for row in &rows {
        let schema: String = row.try_get(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get(1).map_err(|e| e.to_string())?;
        if schema == target {
            in_target.insert(table);
        } else {
            in_legacy.insert(table);
        }
    }
    Ok((in_target, in_legacy))
}

/// The six tables, in [`ATLAS_MANAGED_TABLES`] (FK) order, that exist in
/// neither schema.
fn absent_from_both(in_target: &HashSet<String>, in_legacy: &HashSet<String>) -> Vec<&'static str> {
    ATLAS_MANAGED_TABLES
        .iter()
        .copied()
        .filter(|t| !in_target.contains(*t) && !in_legacy.contains(*t))
        .collect()
}

/// Create any Atlas-owned table missing from `atlas_managed` (and absent from
/// `project`) from the bundled dump. Never fails: an error is logged and boot
/// continues, exactly as a table left behind by the move does.
pub async fn create_missing_atlas_managed_tables(client: &mut Client) -> Vec<&'static str> {
    match create_missing_in(
        client,
        crate::embedded_pg::CANONICAL_SCHEMA_SQL,
        LEGACY_SCHEMA,
        ATLAS_MANAGED_SCHEMA,
    )
    .await
    {
        Ok(created) => {
            if !created.is_empty() {
                info!(
                    tables = ?created,
                    "atlas_managed: created missing Atlas-owned tables from the bundled schema"
                );
            }
            created
        }
        Err(e) => {
            error!("atlas_managed: creating missing Atlas-owned tables failed: {e}");
            Vec::new()
        }
    }
}

/// [`create_missing_atlas_managed_tables`] between arbitrary schemas and dump
/// text, so the Postgres-backed tests can use scratch schemas.
pub(crate) async fn create_missing_in(
    client: &mut Client,
    dump: &str,
    legacy: &str,
    target: &str,
) -> Result<Vec<&'static str>, String> {
    // Steady state: one catalog read, no lock.
    let (in_target, in_legacy) = presence(client, legacy, target).await?;
    if absent_from_both(&in_target, &in_legacy).is_empty() {
        return Ok(Vec::new());
    }

    let objects = atlas_managed_objects(dump)?;
    let tx = client
        .transaction()
        .await
        .map_err(|e| format!("BEGIN failed: {e}"))?;
    tx.batch_execute(&format!(
        "SET LOCAL lock_timeout = '{}ms'; SET LOCAL statement_timeout = '{}ms';",
        DEFAULT_LOCK_TIMEOUT.as_millis(),
        STATEMENT_TIMEOUT.as_millis()
    ))
    .await
    .map_err(|e| format!("session settings failed: {e}"))?;
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&MOVE_LOCK_KEY])
        .await
        .map_err(|e| format!("advisory lock failed: {e}"))?;

    // Re-read under the lock: a move or a peer's create that held it may have
    // changed what is missing.
    let (in_target, in_legacy) = presence(&tx, legacy, target).await?;
    let missing = absent_from_both(&in_target, &in_legacy);
    if missing.is_empty() {
        tx.commit()
            .await
            .map_err(|e| format!("COMMIT failed: {e}"))?;
        return Ok(missing);
    }
    for table in &missing {
        if !objects
            .iter()
            .any(|o| o.kind == ObjectKind::Table && o.table == *table)
        {
            return Err(format!(
                "the bundled schema has no CREATE TABLE for {ATLAS_MANAGED_SCHEMA}.{table}"
            ));
        }
    }

    let parents: Vec<&str> = ATLAS_MANAGED_TABLES
        .iter()
        .copied()
        .filter(|t| in_target.contains(*t) || missing.contains(t))
        .collect();
    let skipped = skipped_foreign_keys(&objects, &missing, &parents);
    if !skipped.is_empty() {
        warn!(
            fks = ?skipped,
            "atlas_managed: creating tables without FKs whose parent is not in {target} \
             (its legacy copy is still in {legacy})"
        );
    }

    tx.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS \"{target}\""))
        .await
        .map_err(|e| format!("CREATE SCHEMA {target} failed: {e}"))?;
    for sql in creation_statements(&objects, &missing, &parents, target) {
        tx.batch_execute(&sql)
            .await
            .map_err(|e| format!("{e}: while running {}", sql.trim()))?;
    }
    tx.commit()
        .await
        .map_err(|e| format!("COMMIT failed: {e}"))?;
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = crate::embedded_pg::CANONICAL_SCHEMA_SQL;

    /// The bundled dump parses, and for every one of the six tables it yields
    /// exactly one CREATE TABLE and a primary key — the property the boot step
    /// depends on. Regenerating the dump cannot silently empty the extractor,
    /// and a schema.hcl change that adds an object kind this module cannot
    /// order fails here.
    #[test]
    fn bundled_dump_carries_every_atlas_managed_table() {
        let objects = atlas_managed_objects(DUMP).expect("bundled dump parses");
        for table in ATLAS_MANAGED_TABLES {
            let creates: Vec<_> = objects
                .iter()
                .filter(|o| o.kind == ObjectKind::Table && o.table == table)
                .collect();
            assert_eq!(creates.len(), 1, "CREATE TABLEs for atlas_managed.{table}");
            assert!(
                creates[0]
                    .sql
                    .starts_with(&format!("CREATE TABLE atlas_managed.{table} (")),
                "{}",
                creates[0].sql
            );
            assert!(
                objects.iter().any(|o| o.kind == ObjectKind::Constraint
                    && o.table == table
                    && o.sql.contains("PRIMARY KEY")),
                "no primary key for atlas_managed.{table}"
            );
        }
        for o in &objects {
            assert!(
                ATLAS_MANAGED_TABLES.contains(&o.table.as_str()),
                "object for unknown table {:?}: {}",
                o.table,
                o.sql
            );
            assert!(o.sql.trim_end().ends_with(';'), "unterminated: {}", o.sql);
            if let Some(parent) = &o.parent {
                assert!(
                    ATLAS_MANAGED_TABLES.contains(&parent.as_str()),
                    "FK to unknown table {parent:?}: {}",
                    o.sql
                );
            }
        }
    }

    #[test]
    fn statements_are_ordered_tables_then_keys_then_indexes_then_fks() {
        let objects = atlas_managed_objects(DUMP).unwrap();
        let stmts = creation_statements(
            &objects,
            &ATLAS_MANAGED_TABLES,
            &ATLAS_MANAGED_TABLES,
            "scratch",
        );
        let first = |needle: &str| stmts.iter().position(|s| s.contains(needle));
        let last = |needle: &str| stmts.iter().rposition(|s| s.contains(needle));
        assert!(last("CREATE TABLE") < first("ADD CONSTRAINT"));
        assert!(last("PRIMARY KEY") < first("CREATE INDEX"));
        assert!(last("CREATE INDEX") < first("FOREIGN KEY"));
        assert!(stmts.iter().all(|s| !s.contains("atlas_managed.")));
        assert!(stmts
            .iter()
            .any(|s| s.contains("REFERENCES scratch.regression_runs(id)")));
    }

    #[test]
    fn only_the_requested_tables_are_selected() {
        let objects = atlas_managed_objects(DUMP).unwrap();
        let stmts = creation_statements(
            &objects,
            &["spec_proposals", "proposal_events"],
            &ATLAS_MANAGED_TABLES,
            ATLAS_MANAGED_SCHEMA,
        );
        assert!(stmts
            .iter()
            .all(|s| s.contains("spec_proposals") || s.contains("proposal_events")));
        assert!(stmts
            .iter()
            .any(|s| s.contains("CREATE TABLE atlas_managed.spec_proposals")));
        assert!(stmts
            .iter()
            .any(|s| s.contains("CREATE UNIQUE INDEX spec_proposals_kind_target_uniq")));
    }

    /// A child created while its parent is not in the target (its legacy copy
    /// could not be moved) gets its table, keys and indexes but not the FK
    /// that would fail the whole transaction.
    #[test]
    fn an_fk_whose_parent_will_not_exist_is_left_out() {
        let objects = atlas_managed_objects(DUMP).unwrap();
        let tables = ["regression_diagnoses"];
        let parents = ["regression_diagnoses"];
        let stmts = creation_statements(&objects, &tables, &parents, "scratch");
        assert!(stmts
            .iter()
            .any(|s| s.contains("CREATE TABLE scratch.regression_diagnoses")));
        assert!(stmts.iter().all(|s| !s.contains("FOREIGN KEY")));
        assert_eq!(
            skipped_foreign_keys(&objects, &tables, &parents),
            vec!["regression_diagnoses -> regression_runs".to_string()]
        );

        let with_parent = ["regression_runs", "regression_diagnoses"];
        let stmts = creation_statements(&objects, &tables, &with_parent, "scratch");
        assert!(stmts
            .iter()
            .any(|s| s.contains("REFERENCES scratch.regression_runs(id)")));
        assert!(skipped_foreign_keys(&objects, &tables, &with_parent).is_empty());
    }

    #[test]
    fn absent_from_both_keeps_fk_order_and_excludes_legacy_copies() {
        let in_target: HashSet<String> = ["regression_suites".to_string()].into();
        let in_legacy: HashSet<String> = ["proposal_events".to_string()].into();
        assert_eq!(
            absent_from_both(&in_target, &in_legacy),
            vec![
                "regression_runs",
                "regression_diagnoses",
                "regression_assertion_executions",
                "spec_proposals",
            ]
        );
    }

    #[test]
    fn an_unknown_object_kind_is_refused() {
        let dump = "--\n-- Name: s; Type: SEQUENCE; Schema: atlas_managed; Owner: -\n--\n\n\
                    CREATE SEQUENCE atlas_managed.s;\n";
        let err = atlas_managed_objects(dump).unwrap_err();
        assert!(err.contains("SEQUENCE"), "{err}");
    }

    #[test]
    fn a_multi_line_statement_is_captured_whole() {
        let dump = "--\n-- Name: t t_fkey; Type: FK CONSTRAINT; Schema: atlas_managed; Owner: -\n\
                    --\n\nALTER TABLE ONLY atlas_managed.t\n    ADD CONSTRAINT t_fkey \
                    FOREIGN KEY (a) REFERENCES atlas_managed.p(id);\n\n\n--\n-- Name: other\n";
        let objects = atlas_managed_objects(dump).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].table, "t");
        assert_eq!(objects[0].kind, ObjectKind::ForeignKey);
        assert_eq!(objects[0].parent.as_deref(), Some("p"));
        assert!(objects[0].sql.contains("REFERENCES atlas_managed.p(id);"));
    }

    /// Postgres-backed. Scratch schemas on the `DATABASE_URL` database for the
    /// selection rules; a throwaway database for the boot path.
    ///
    /// Run: `cargo test --features pg_integration_tests -- atlas_managed_provision::tests::pg`
    #[cfg(feature = "pg_integration_tests")]
    mod pg {
        use super::super::*;
        use crate::database::pg::{test_support::FreshTestDatabase, PgDb};

        async fn connect(url: &str) -> Client {
            let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
                .await
                .expect("connect to the test database");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            client
        }

        fn database_url() -> String {
            std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string())
        }

        async fn tables_in(client: &Client, schema: &str) -> Vec<String> {
            client
                .query(
                    "SELECT tablename::text FROM pg_catalog.pg_tables \
                     WHERE schemaname = $1 ORDER BY 1",
                    &[&schema],
                )
                .await
                .unwrap()
                .iter()
                .map(|r| r.try_get(0).expect("tablename"))
                .collect()
        }

        async fn count(client: &Client, sql: &str, args: &[&str]) -> i64 {
            let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = args
                .iter()
                .map(|a| a as &(dyn tokio_postgres::types::ToSql + Sync))
                .collect();
            client
                .query_one(sql, &params)
                .await
                .unwrap()
                .try_get(0)
                .unwrap()
        }

        /// An older cluster after the move: the regression tables are in the
        /// target except `regression_runs`, whose legacy copy the move could
        /// not carry; `proposal_events` is also legacy-only; `spec_proposals`
        /// and `regression_diagnoses` are nowhere. Only the last two are
        /// created, `regression_diagnoses` without its FK to the absent
        /// parent; a second run creates nothing.
        #[tokio::test(flavor = "multi_thread")]
        async fn creates_only_the_tables_absent_from_both_schemas() {
            let mut client = connect(&database_url()).await;
            let tag: String = uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(12)
                .collect();
            let legacy = format!("amp_src_{tag}");
            let target = format!("amp_dst_{tag}");

            let dump = crate::embedded_pg::CANONICAL_SCHEMA_SQL;
            let objects = atlas_managed_objects(dump).unwrap();
            let in_target = ["regression_suites", "regression_assertion_executions"];
            let mut setup = format!(
                "CREATE SCHEMA {legacy}; CREATE SCHEMA {target}; \
                 CREATE TABLE {legacy}.regression_runs (id uuid PRIMARY KEY); \
                 CREATE TABLE {legacy}.proposal_events (id text PRIMARY KEY);"
            );
            for s in creation_statements(&objects, &in_target, &in_target, &target) {
                setup.push_str(&s);
            }
            client.batch_execute(&setup).await.unwrap();

            let created = create_missing_in(&mut client, dump, &legacy, &target)
                .await
                .unwrap();
            assert_eq!(created, vec!["regression_diagnoses", "spec_proposals"]);
            let tables = tables_in(&client, &target).await;
            assert!(tables.contains(&"spec_proposals".to_string()));
            assert!(tables.contains(&"regression_diagnoses".to_string()));
            assert!(!tables.contains(&"proposal_events".to_string()));
            assert!(!tables.contains(&"regression_runs".to_string()));

            // The unique index the dedup path depends on came with it.
            let idx = count(
                &client,
                "SELECT count(*) FROM pg_catalog.pg_indexes \
                 WHERE schemaname = $1 AND indexname = 'spec_proposals_kind_target_uniq'",
                &[&target],
            )
            .await;
            assert_eq!(idx, 1);
            // No FK was attempted against the missing parent.
            let fks = count(
                &client,
                "SELECT count(*) FROM pg_catalog.pg_constraint k \
                 JOIN pg_catalog.pg_namespace n ON n.oid = k.connamespace \
                 WHERE n.nspname = $1 AND k.contype = 'f'",
                &[&target],
            )
            .await;
            assert_eq!(fks, 0);

            // Idempotent.
            let again = create_missing_in(&mut client, dump, &legacy, &target)
                .await
                .unwrap();
            assert!(again.is_empty());

            client
                .batch_execute(&format!(
                    "DROP SCHEMA {legacy} CASCADE; DROP SCHEMA {target} CASCADE;"
                ))
                .await
                .unwrap();
        }

        /// The boot path end to end: a provisioned database that lacks
        /// `spec_proposals` / `proposal_events` (an embedded cluster built
        /// from a pre-#1815 dump) has both, with their primary keys and
        /// indexes, after `PgDb::new`.
        #[tokio::test(flavor = "multi_thread")]
        async fn boot_recreates_dropped_proposal_tables() {
            let db = FreshTestDatabase::create("amp_boot").await;
            let client = connect(&db.url).await;
            client
                .batch_execute(
                    "DROP TABLE atlas_managed.proposal_events; \
                     DROP TABLE atlas_managed.spec_proposals;",
                )
                .await
                .unwrap();
            assert!(!tables_in(&client, ATLAS_MANAGED_SCHEMA)
                .await
                .contains(&"spec_proposals".to_string()));

            let booted = PgDb::new(&db.url).await.map(|_| ());

            let tables = tables_in(&client, ATLAS_MANAGED_SCHEMA).await;
            let pks = count(
                &client,
                "SELECT count(*) FROM pg_catalog.pg_constraint k \
                 JOIN pg_catalog.pg_class c ON c.oid = k.conrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = 'atlas_managed' AND k.contype = 'p' \
                   AND c.relname IN ('spec_proposals', 'proposal_events')",
                &[],
            )
            .await;
            let indexes = count(
                &client,
                "SELECT count(*) FROM pg_catalog.pg_indexes \
                 WHERE schemaname = 'atlas_managed' AND indexname IN ( \
                   'spec_proposals_kind_target_uniq', 'spec_proposals_status_idx', \
                   'proposal_events_proposal_at_idx', 'proposal_events_type_at_idx', \
                   'idx_proposal_events_app_id', 'idx_proposal_events_app_id_at_ms')",
                &[],
            )
            .await;
            drop(client);
            db.drop().await;

            assert!(booted.is_ok(), "{booted:?}");
            assert!(tables.contains(&"spec_proposals".to_string()), "{tables:?}");
            assert!(
                tables.contains(&"proposal_events".to_string()),
                "{tables:?}"
            );
            assert_eq!(pks, 2);
            assert_eq!(indexes, 6);
        }
    }
}
