#!/usr/bin/env bash
# Apply atlas/schema.hcl (`--env runner_pilot`) to the database at $1.
#
# Atlas is the declared owner of `atlas_managed` and `orchestration`, and no
# alembic revision creates or moves their tables. So every pipeline that
# builds a database to generate code or snapshots from -- clorinde bindings,
# schema.pg.sql.generated -- runs this AFTER `alembic upgrade head`, or the
# output would be missing the Atlas-owned tables entirely.
#
# Refuses (without applying anything) when the plan contains a DROP: that
# means an object Atlas does not declare lives in an Atlas-owned schema, and
# the ownership check (.github/workflows/atlas-schema-check.yml) is the place
# to resolve it, not a codegen run.
#
# Usage:
#   bash atlas/scripts/apply_to.sh 'postgresql://user:pass@localhost:5433/db'
#
# Needs docker (or ATLAS_BIN plus a native psql). Pass the URL as the host
# sees it; when the database runs in a named docker container, also set
# ATLAS_PG_CONTAINER=<name> so the helper containers join that container's
# network (required on Docker Desktop, which has no host networking) -- see
# lib.sh. A throwaway dev database is created beside the target and dropped
# on exit. Exit: 0 applied (or already
# in sync), 1 refused or failed.

set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <database-url>" >&2; exit 1; }

# shellcheck source=atlas/scripts/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
atlas_init

ATLAS_LIVE_URL="$(atlas_normalize_url "$1")"
export ATLAS_LIVE_URL
atlas_provision_dev_db "$ATLAS_LIVE_URL"

atlas_run schema apply --env runner_pilot --dry-run >"$atlas_work/plan.sql" \
  || atlas_fail "atlas schema apply --dry-run errored"
drops="$(atlas_plan_drops "$atlas_work/plan.sql")"
if [ -n "$drops" ]; then
  cat "$atlas_work/plan.sql"
  atlas_fail "refusing to apply: the plan DROPs something in an Atlas-owned schema (run atlas/scripts/check_schema.sh and resolve it there)"
fi

atlas_run schema apply --env runner_pilot --auto-approve >"$atlas_work/apply.log" \
  || { cat "$atlas_work/apply.log"; atlas_fail "atlas schema apply errored"; }
echo "[atlas] applied schema.hcl ($(atlas_statements "$atlas_work/plan.sql" | grep -c ';' || true) statement(s))"
