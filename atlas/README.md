# Atlas — the runner's declaratively managed schemas

Atlas ([atlasgo.io](https://atlasgo.io), Community edition) declaratively
manages two Postgres schemas, and owns each of them **wholly**:

| Schema | Tables |
|--------|--------|
| `atlas_managed` | `regression_suites`, `regression_runs`, `regression_diagnoses`, `regression_assertion_executions`, `spec_proposals`, `proposal_events` |
| `orchestration` | `runs`, `subtasks` (the Approach-D conductor ledger) |

Every other schema — `project`, `coord`, `agent`, `auth`, … — is authored by
qontinui-web's alembic chain and is **outside Atlas's scope entirely**. That
includes `project.apps`, which alembic creates and the runner self-heals.
alembic's `env.py` skips `atlas_managed` and `orchestration` at schema level,
so neither system ever reasons about the other's objects.

## Why there is no exclude list

A schema that is not listed in `schemas` (atlas.hcl) is invisible to both
`schema diff` and `schema apply`, so none of its objects can ever become a DROP
candidate. The previous layout scoped Atlas to `project` and `coord` and then
excluded ~630 alembic-owned objects one pattern at a time, which a nightly job
had to keep regenerating. Moving Atlas's tables into a schema it owns outright
replaced that with a structural guarantee (plan
`2026-05-14-atlas-wave-6-triage`).

The rule that makes it hold: **every schema in `schemas` holds only objects
`schema.hcl` declares.** Nothing else may create objects in `atlas_managed` or
`orchestration`.

## Files

| File | Purpose |
|------|---------|
| `atlas.hcl` | Atlas project config. The `runner_pilot` env scopes Atlas to `schemas = ["atlas_managed", "orchestration"]`. |
| `schema.hcl` | The desired state of every object in those two schemas. |
| `scripts/check_schema.sh` | The ownership check (below). |
| `scripts/apply_to.sh` | Applies `schema.hcl` to a database (refusing any plan with a DROP). Every codegen pipeline runs it after `alembic upgrade head`. |
| `scripts/lib.sh` | Shared helpers: containerised Atlas and psql, the throwaway no-`public` dev database. |

## Codegen databases need Atlas applied

No alembic revision creates or moves the Atlas-owned tables. A database built
by `alembic upgrade head` alone has nothing in `atlas_managed`, but it does
still carry four legacy `project.regression_*` tables from frozen revision
`f9d3e8a4c1b6`. So every pipeline that builds a database to generate from runs
`scripts/apply_to.sh` after alembic: `clorinde-bindings-fresh.yml`,
`schema-pg-sql-fresh.yml`, `schema-pg-sql-freshness-nightly.yml`, and
`src-tauri/scripts/regenerate_schema_pg_sql.sh`. The regeneration script also
excludes the legacy `project.regression_*` copies from its dump, so
`schema.pg.sql.generated` (and every fresh embedded cluster built from it)
carries all six tables in `atlas_managed` and none in `project`.

On a live database the runner reconciles any legacy copies at boot (see the
last section).

## The ownership check

`.github/workflows/atlas-schema-check.yml` applies qontinui-web's
`alembic upgrade head` to a scratch Postgres and runs
`scripts/check_schema.sh`, which asserts:

1. the Atlas dev database has no `public` schema (see below; the script
   provisions a throwaway one unless `ATLAS_DEV_URL` names one);
2. the plan `schema apply --env runner_pilot` would run contains **no DROP** —
   a DROP means something Atlas does not declare lives in a schema it owns;
3. after applying it, a second `schema diff` plans **nothing** (idempotence);
4. every table `schema.hcl` declares **exists** afterwards (non-vacuity: a run
   that read no HCL would otherwise pass 2 and 3 trivially).

It runs on PRs touching `atlas/**`, nightly (a qontinui-web migration can put
an object into an Atlas-owned schema without touching this repo), and on
`workflow_dispatch`. It is deliberately not a required check. Locally:

```bash
ATLAS_LIVE_URL='postgres://user:pass@localhost:5433/alembic_head_db' \
  bash atlas/scripts/check_schema.sh
```

It needs only docker: Atlas runs from the pinned Community image and `psql`
from `postgres:16` when no native `psql` is on `PATH` (`ATLAS_BIN=/path/to/atlas`
uses a native Atlas instead). It **applies** the plan to `ATLAS_LIVE_URL`, so
point it at a scratch database. It cannot see objects the runner's boot-time
self-heal creates, because CI runs alembic only.

## The dev-database precondition

Atlas normalises the HCL in a *dev* database. That database must have **no
`public` schema**, or `schema apply` aborts its post-apply verification after
an otherwise clean plan:

```
Abort: the planned state does not match the desired state after applying the file:
  -CREATE SCHEMA IF NOT EXISTS "public";
```

`scripts/lib.sh` creates a dedicated empty database beside the target, drops
its `public` schema, and drops the database again on exit. The target
database's own `public` schema is irrelevant.

## Applying (manual)

```bash
docker run --rm --network host -v "${PWD}/atlas:/work" -w /work \
  -e ATLAS_LIVE_URL -e ATLAS_DEV_URL \
  arigaio/atlas:1.3.3-community schema apply --env runner_pilot --dry-run   # preview
docker run --rm --network host -v "${PWD}/atlas:/work" -w /work \
  -e ATLAS_LIVE_URL -e ATLAS_DEV_URL \
  arigaio/atlas:1.3.3-community schema apply --env runner_pilot
```

Always go through `--env runner_pilot`: it is what binds the two-schema scope.
Refuse any preview that contains a `DROP` you did not intend.

## Relationship to the runner's boot-time self-heal

`database/pg/mod.rs::verify_and_provision` does three things that touch these
schemas, and all three must stay consistent with `schema.hcl`:

- it imperatively creates `orchestration.runs` / `orchestration.subtasks` so a
  fresh database without Atlas applied still boots the conductor;
- it moves any leftover `project.<t>` copy of the six `atlas_managed` tables
  into `atlas_managed` — `migrate_atlas_managed_tables`. A legacy-only table is
  moved with `SET SCHEMA` (its FKs re-pointed at `atlas_managed` parents); a
  table in both schemas is merged, and the old copy is dropped only when every
  legacy row is present and identical in the target. Anything else is left in
  place and logged at ERROR; the pass never fails boot.
- then it creates any of the six a database lacks entirely —
  `atlas_managed_provision::create_missing_atlas_managed_tables`. An embedded
  cluster built from a dump older than `spec_proposals` / `proposal_events`
  never gets them otherwise, because the canonical schema is applied only to a
  fresh cluster. The DDL is not a second copy of `schema.hcl`: it is the
  `atlas_managed` objects of the bundled `schema.pg.sql.generated`, picked out
  by pg_dump's `-- Name: …; Schema: atlas_managed` headers. A table with a
  legacy `project` copy still in place is not created, so a leftover the move
  could not carry is never hidden behind an empty twin (and a child created
  beside such a leftover parent is created without that one FK). It runs in
  one transaction under the move's own advisory lock.

All of that DDL runs under one session-level provisioning advisory lock, so
runners booting against one database at once (a primary and a temp runner on
a shared embedded cluster) provision one after another instead of racing on
`CREATE … IF NOT EXISTS`.

The move looks in `project` only. A dev Docker volume initialised before
qontinui-web#1546 may also hold four orphaned `public.regression_*` tables:
the old `init-scripts/01-create-runner-schema.sql` created them unqualified at
initdb time. Nothing reads them, and `public` is outside both the runner's and
Atlas's authority, so they are deliberately left alone; recreating the volume
removes them.

pgvector and pgcrypto stay imperatively bootstrapped there too: Atlas
Community cannot own extensions.
