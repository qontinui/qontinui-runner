# Atlas — runner schema-half pilot

Atlas ([atlasgo.io](https://atlasgo.io)) declaratively manages the
**pilot-owned** slice of the canonical Postgres schema (Row 3, Wave 1). The
rest of the `project.*` and `coord.*` schemas is owned by qontinui-web's
alembic chain and coord — **not** Atlas.

## Files

| File | Purpose |
|------|---------|
| `atlas.hcl` | Atlas project config. The `runner_pilot` env scopes Atlas to `schemas = ["project","coord"]` minus the `exclude.txt` list. |
| `schema.hcl` | The desired-state HCL for the Atlas-owned pilot tables. |
| `exclude.txt` | The `--exclude` list. Atlas Community can't `--include` (Pro-only), so every *non*-pilot table/sequence/view/MV/enum in `project`/`coord`/`orchestration` is listed here so Atlas ignores it. |
| `scripts/regen_exclude.ps1` | Regenerates `exclude.txt` from the live schema. Also the single declaration of the pilot set (`$PilotTables`). |
| `scripts/check_pilot_consistency.ps1` | DB-free guard that the four files above agree about who owns what. |
| `scripts/tests/` | Stubbed-psql regression test for the regen script's exit-code contract. |

## Who owns which table

Four files encode "which tables does Atlas manage?", and they must agree:

| | Says |
|---|---|
| `schema.hcl` | the desired state — the tables Atlas **manages** |
| `atlas.hcl` (`schemas`) | which schemas Atlas is **scoped** to |
| `exclude.txt` | what Atlas **ignores** |
| `regen_exclude.ps1` (`$PilotTables`) | what the regen **carves out** of `exclude.txt` |

A table declared in `schema.hcl` must **not** appear in `exclude.txt` — that
combination means Atlas silently ignores a table the HCL claims to own, and the
declaration is dead. The reverse (carved out but never declared) leaves a table
neither managed nor excluded.

`scripts/check_pilot_consistency.ps1` asserts all of this without a database, so
it runs on every trigger and its verdict does **not** depend on qontinui-web's
migration state:

```bash
pwsh atlas/scripts/check_pilot_consistency.ps1
```

One deliberate exception is recorded by name in `$ExcludedDeclarations`:
**`project.apps`** is declared in `schema.hcl` but stays excluded, because
qontinui-web's alembic chain co-authors it and the runner's own self-heal adds
six columns the HCL does not declare. See the block comment above `table "apps"`
in `schema.hcl`. The checker fails if it ever leaves `exclude.txt` while that is
still true.

## The exclude-list footgun — regenerate after every schema change

`exclude.txt` lists every table Atlas must **leave alone**. When a qontinui-web
alembic migration **adds** a `project.*` or `coord.*` table and `exclude.txt` is
not regenerated, that new table is no longer excluded — so the next
`atlas schema apply` sees a live table absent from `schema.hcl` and tries to
**`DROP` it**. This is a data-loss-class hazard.

Two guards exist:

1. **CI freshness check** — `.github/workflows/atlas-exclude-fresh.yml`
   regenerates the list against a fresh `alembic upgrade head` schema and fails
   on drift. It runs nightly (catches web-side drift ≤24h), on
   `workflow_dispatch`, and on PRs that touch `atlas/**`. It is intentionally
   **not** a required per-PR check (a web-side migration would otherwise red
   every unrelated runner PR and stall the merge train).

2. **Manual pre-apply check** (do this yourself) — **always run the check
   before `atlas schema apply`:**

   ```bash
   pwsh atlas/scripts/regen_exclude.ps1 -Check
   ```

   | Exit | Meaning |
   |------|---------|
   | `0` | the list is fresh — safe to apply |
   | `2` | **drift** — a table was added or removed since the last regen |
   | `1` | **failure** — DB unreachable, a query failed, or the zero-pattern guard tripped. Nothing was established; do **not** apply |

   The 1-vs-2 split is a contract, not a detail: CI self-heals (auto-commits a
   refreshed list) on `2` and must never do so on `1`, since a list built from a
   failed read is exactly the data-loss footgun this guard exists to prevent. It
   is pinned by `scripts/tests/test_regen_exit_contract.ps1`.

   On drift, regenerate and commit first:

   ```bash
   pwsh atlas/scripts/regen_exclude.ps1        # rewrites atlas/exclude.txt
   git add atlas/exclude.txt && git commit -m "chore(atlas): regen exclude list"
   ```

## regen_exclude.ps1 — modes & transport

```
pwsh atlas/scripts/regen_exclude.ps1            # rewrite exclude.txt in place
pwsh atlas/scripts/regen_exclude.ps1 -Check     # diff vs committed; 0 fresh / 2 drift / 1 failed
pwsh atlas/scripts/regen_exclude.ps1 -PrintPilotSet   # the pilot set, no DB
```

DB transport:

- Default: `-Container qontinui-canonical-postgres` runs `psql` via
  `docker exec` against the local canonical-PG container.
- `-Container ""` uses a **native** `psql` on the host at `-PgHost:-PgPort`
  (default `localhost:5433`). CI uses this path against its `services: postgres`
  container.

The script writes `exclude.txt` with **LF** line endings regardless of host OS
(`.gitattributes` also declares `*.txt text eol=lf`), so the `-Check` diff only
ever reflects real schema drift, never EOL noise.

## Applying (manual)

```bash
docker run --rm --network host -v "${PWD}/atlas:/atlas" -w /atlas \
  arigaio/atlas:latest schema diff --env runner_pilot   # preview
# ...run the -Check guard above, then:
docker run --rm --network host -v "${PWD}/atlas:/atlas" -w /atlas \
  arigaio/atlas:latest schema apply --env runner_pilot
```
