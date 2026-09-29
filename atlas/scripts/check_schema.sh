#!/usr/bin/env bash
# Atlas ownership check: Atlas owns `atlas_managed` and `orchestration` WHOLLY.
#
# Run against a database that already has qontinui-web's alembic chain applied
# (the "live" DB). Four assertions, in order:
#
#   1. PRECONDITION -- the dev DB has no `public` schema. With one, `schema
#      apply` aborts its post-apply verification after an otherwise clean plan
#      (spike Q1-1). A dev DB is provisioned (and dropped) automatically unless
#      ATLAS_DEV_URL names one, in which case it is checked, never modified.
#   2. NO DROP -- the plan `schema apply` would run contains no DROP of any
#      kind. A DROP means an object Atlas did not declare lives in a schema it
#      owns -- in CI, one an alembic migration created in `atlas_managed` or
#      `orchestration` -- and a real apply would destroy it. (CI runs alembic
#      only, not the runner's boot-time self-heal, so it cannot see objects
#      the runner itself creates.) The plan is then applied.
#   3. IDEMPOTENT -- a second `schema diff` plans nothing. Anything left means
#      a declaration Atlas cannot converge.
#   4. NON-VACUOUS -- every table schema.hcl declares exists as a table
#      afterwards. A green run that read no HCL (the fail-open shape spike
#      Q2-1 measured) would otherwise pass checks 2 and 3 trivially.
#
# Usage:
#   ATLAS_LIVE_URL=postgres://...alembic-head-db bash atlas/scripts/check_schema.sh
#
# It APPLIES the plan to ATLAS_LIVE_URL, so point it at a scratch database.
# Needs docker (or ATLAS_BIN plus a native psql). When the database runs in
# a named docker container, set ATLAS_PG_CONTAINER=<name> so the helper
# containers join its network (see lib.sh; needed on Docker Desktop).
# Exit: 0 all checks pass, 1 a check failed or a tool errored.

set -euo pipefail

# shellcheck source=atlas/scripts/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
atlas_init

[ -n "${ATLAS_LIVE_URL:-}" ] || atlas_fail "ATLAS_LIVE_URL must name the alembic-head database"
ATLAS_LIVE_URL="$(atlas_normalize_url "$ATLAS_LIVE_URL")"
export ATLAS_LIVE_URL

# --- 1. dev DB precondition ---------------------------------------------------
if [ -n "${ATLAS_DEV_URL:-}" ]; then
  ATLAS_DEV_URL="$(atlas_normalize_url "$ATLAS_DEV_URL")"
  export ATLAS_DEV_URL
  public_count="$(atlas_psql "$ATLAS_DEV_URL" -Atqc \
    "SELECT count(*) FROM pg_namespace WHERE nspname = 'public'")" \
    || atlas_fail "could not query the dev database"
  [ "$public_count" = "0" ] \
    || atlas_fail "the dev database still has a public schema; drop it first (DROP SCHEMA public CASCADE on the DEV db only) -- schema apply aborts post-apply verification otherwise"
else
  atlas_provision_dev_db "$ATLAS_LIVE_URL"
fi
echo "ok  dev database has no public schema"

# --- 2. planned changes contain no DROP --------------------------------------
# Atlas prints its notices on stderr; the plan is on stdout.
atlas_run schema apply --env runner_pilot --dry-run >"$atlas_work/plan.sql" \
  || atlas_fail "atlas schema apply --dry-run errored"
echo "--- planned changes against the alembic-head database ---"
cat "$atlas_work/plan.sql"
echo "---------------------------------------------------------"
drops="$(atlas_plan_drops "$atlas_work/plan.sql")"
if [ -n "$drops" ]; then
  echo "$drops"
  atlas_fail "the plan DROPs something: an object schema.hcl does not declare lives in an Atlas-owned schema (atlas_managed / orchestration). Declare it in atlas/schema.hcl, or move it out of the schema on the side that created it."
fi
echo "ok  plan contains no DROP ($(atlas_statements "$atlas_work/plan.sql" | grep -c ';' || true) statement(s))"

atlas_run schema apply --env runner_pilot --auto-approve >"$atlas_work/apply.log" \
  || { cat "$atlas_work/apply.log"; atlas_fail "atlas schema apply errored"; }
echo "ok  plan applied"

# --- 3. idempotence -----------------------------------------------------------
atlas_run schema diff --env runner_pilot --from env://url --to env://src >"$atlas_work/diff.sql" \
  || atlas_fail "atlas schema diff errored"
if [ -n "$(atlas_statements "$atlas_work/diff.sql")" ]; then
  cat "$atlas_work/diff.sql"
  atlas_fail "a second diff after apply still plans changes: schema.hcl declares something Atlas cannot converge"
fi
echo "ok  second diff is empty ($(tr -d '\n' <"$atlas_work/diff.sql"))"

# --- 4. non-vacuity -----------------------------------------------------------
declared="$(atlas_declared_tables)"
[ -n "$declared" ] || atlas_fail "parsed no table declarations out of atlas/schema.hcl -- the check itself is broken"

missing=0
while IFS= read -r qualified; do
  schema="${qualified%%.*}"
  table="${qualified#*.}"
  present="$(atlas_psql "$ATLAS_LIVE_URL" -Atqc \
    "SELECT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
      WHERE n.nspname = '$schema' AND c.relname = '$table' AND c.relkind IN ('r', 'p'))")" \
    || atlas_fail "could not query the live database for $qualified"
  if [ "$present" = "t" ]; then
    echo "ok  $qualified exists"
  else
    echo "::error::$qualified is declared in schema.hcl but no such table exists after apply"
    missing=$((missing + 1))
  fi
done <<<"$declared"
[ "$missing" -eq 0 ] || atlas_fail "$missing declared table(s) absent after apply -- Atlas did not read or apply schema.hcl"

echo "PASS: Atlas owns atlas_managed + orchestration cleanly ($(wc -l <<<"$declared") tables)"
