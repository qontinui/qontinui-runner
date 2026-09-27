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

## The ownership check

`.github/workflows/atlas-schema-check.yml` applies qontinui-web's
`alembic upgrade head` to a scratch Postgres and runs
`scripts/check_schema.sh`, which asserts:

1. the Atlas dev database has no `public` schema (see below);
2. the plan `schema apply --env runner_pilot` would run contains **no DROP** —
   a DROP means something Atlas does not declare lives in a schema it owns;
3. after applying it, a second `schema diff` plans **nothing** (idempotence);
4. every table `schema.hcl` declares **exists** afterwards (non-vacuity: a run
   that read no HCL would otherwise pass 2 and 3 trivially).

It runs on PRs touching `atlas/**`, nightly (a qontinui-web migration can put
an object into an Atlas-owned schema without touching this repo), and on
`workflow_dispatch`. It is deliberately not a required check. Locally:

```bash
ATLAS_LIVE_URL='postgres://user:pass@localhost:5433/alembic_head_db?sslmode=disable' \
ATLAS_DEV_URL='postgres://user:pass@localhost:5433/empty_dev_db?sslmode=disable' \
  bash atlas/scripts/check_schema.sh
```

It needs `psql` on `PATH` and runs Atlas from the pinned Community docker image
(`ATLAS_BIN=/path/to/atlas` uses a native binary instead). It **applies** the
plan to `ATLAS_LIVE_URL`, so point it at a scratch database.

## The dev-database precondition

Atlas normalises the HCL in a *dev* database. That database must have **no
`public` schema**, or `schema apply` aborts its post-apply verification after
an otherwise clean plan:

```
Abort: the planned state does not match the desired state after applying the file:
  -CREATE SCHEMA IF NOT EXISTS "public";
```

Use a dedicated empty database and `DROP SCHEMA public CASCADE` in it. The
target database's own `public` schema is irrelevant.

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

`database/pg/mod.rs::verify_and_provision` does two things that touch these
schemas, and both must stay consistent with `schema.hcl`:

- it imperatively creates `orchestration.runs` / `orchestration.subtasks` so a
  fresh database without Atlas applied still boots the conductor;
- it moves any leftover `project.<t>` copy of the six `atlas_managed` tables
  into `atlas_managed` (merging, and dropping the old copy only after every row
  is verified present) — `migrate_atlas_managed_tables`.

pgvector and pgcrypto stay imperatively bootstrapped there too: Atlas
Community cannot own extensions.
