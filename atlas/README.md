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
| `exclude.txt` | The `--exclude` list. Atlas Community can't `--include` (Pro-only), so every *non*-pilot table/sequence/view/MV/enum in `project`/`coord` is listed here so Atlas ignores it. |
| `scripts/regen_exclude.ps1` | Regenerates `exclude.txt` from the live schema. |

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

   Exit 0 = the list is fresh, safe to apply. Exit 1 = a table was added or
   removed since the list was last regenerated — regenerate and commit first:

   ```bash
   pwsh atlas/scripts/regen_exclude.ps1        # rewrites atlas/exclude.txt
   git add atlas/exclude.txt && git commit -m "chore(atlas): regen exclude list"
   ```

## regen_exclude.ps1 — modes & transport

```
pwsh atlas/scripts/regen_exclude.ps1            # rewrite exclude.txt in place
pwsh atlas/scripts/regen_exclude.ps1 -Check     # diff vs committed; exit 1 on drift
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
